//! Markdown → ratatui rendering for the transcript: the pulldown-cmark event
//! renderer (`render_markdown_lines{,_with_hits}`), syntect code-block
//! highlighting, table/list/blockquote layout, and the single-line
//! `parse_md_line` used for streaming. Extracted from `tui::app`.
#![allow(clippy::collapsible_match)]

use std::sync::OnceLock;

use pulldown_cmark::{Alignment, CodeBlockKind, Event as MdEvent, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

use crate::tui::app::{LineAnswerHit, LineClickHit};
use crate::tui::render_helpers::truncate_chars;
use crate::tui::theme;

enum MdOpenTag {
    Strong,
    Emphasis,
    Strike,
    Link,
    Heading,
    BlockQuote,
    Paragraph,
    List,
    Item,
    CodeBlock,
    Other,
}

#[derive(Debug, Clone)]
enum MdListState {
    Unordered,
    Ordered(u64),
}

static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
static THEME_SET: OnceLock<ThemeSet> = OnceLock::new();

fn syntect_theme() -> &'static Theme {
    let ts = THEME_SET.get_or_init(ThemeSet::load_defaults);
    ts.themes
        .get("base16-ocean.dark")
        .or_else(|| ts.themes.values().next())
        .expect("syntect theme set must not be empty")
}

fn syntect_style_to_ratatui(style: syntect::highlighting::Style) -> Style {
    let mut out = Style::default().fg(Color::Rgb(
        style.foreground.r,
        style.foreground.g,
        style.foreground.b,
    ));
    if style.font_style.contains(FontStyle::BOLD) {
        out = out.add_modifier(Modifier::BOLD);
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        out = out.add_modifier(Modifier::ITALIC);
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        out = out.add_modifier(Modifier::UNDERLINED);
    }
    out
}

fn render_code_block_lines(
    out: &mut Vec<Line<'static>>,
    hits: &mut Vec<LineAnswerHit>,
    language: Option<String>,
    code: &str,
    code_line_numbers: bool,
) {
    let ps = SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines);
    let is_diff = language
        .as_deref()
        .map(|l| matches!(l.trim().to_ascii_lowercase().as_str(), "diff" | "patch"))
        .unwrap_or(false);
    let syntax = language
        .as_deref()
        .and_then(|lang| ps.find_syntax_by_token(lang))
        .unwrap_or_else(|| ps.find_syntax_plain_text());
    let mut highlighter = HighlightLines::new(syntax, syntect_theme());
    let line_count = code.split('\n').count().max(1);
    let line_num_width = line_count.to_string().len();
    let copy_payload = code.to_string();

    // Language label chip on top of the block, with a click-to-copy affordance.
    if let Some(lang) = language.as_deref().map(str::trim).filter(|l| !l.is_empty()) {
        out.push(Line::from(vec![
            Span::styled(
                format!(" {lang} "),
                Style::default()
                    .fg(Color::Black)
                    .bg(theme::tool())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  copy",
                Style::default()
                    .fg(theme::muted())
                    .add_modifier(Modifier::UNDERLINED),
            ),
        ]));
        hits.push(Some(LineClickHit::CopyText(copy_payload.clone())));
    }

    let code_bg = theme::surface();
    for (idx, raw) in code.split('\n').enumerate() {
        let highlights = if is_diff {
            Vec::new()
        } else {
            highlighter.highlight_line(raw, ps).unwrap_or_default()
        };
        let mut spans: Vec<Span<'static>> = Vec::new();
        if code_line_numbers {
            spans.push(Span::styled(
                format!("{:>width$} │ ", idx + 1, width = line_num_width),
                Style::default().fg(theme::muted()).bg(code_bg),
            ));
        }
        if is_diff {
            let (lane, style) = if raw.starts_with("+++")
                || raw.starts_with("---")
                || raw.starts_with("@@")
                || raw.starts_with("diff ")
                || raw.starts_with("index ")
            {
                ("▌", Style::default().fg(theme::warn()).bg(theme::surface()))
            } else if raw.starts_with('+') {
                (
                    "▌",
                    Style::default().fg(theme::success()).bg(theme::surface()),
                )
            } else if raw.starts_with('-') {
                (
                    "▌",
                    Style::default().fg(theme::error()).bg(theme::surface()),
                )
            } else {
                (
                    "▌",
                    Style::default().fg(theme::muted()).bg(theme::surface()),
                )
            };
            spans.push(Span::styled(format!("{lane} "), style));
            spans.push(Span::styled(raw.to_string(), style));
        } else if highlights.is_empty() {
            spans.push(Span::styled(
                raw.to_string(),
                Style::default().fg(theme::tool()).bg(theme::surface()),
            ));
        } else {
            spans.extend(highlights.into_iter().map(|(style, text)| {
                Span::styled(
                    text.to_string(),
                    syntect_style_to_ratatui(style).bg(theme::surface()),
                )
            }));
        }
        out.push(Line::from(spans));
        hits.push(Some(LineClickHit::CopyText(copy_payload.clone())));
    }
}

fn flush_md_render_line(
    out: &mut Vec<Line<'static>>,
    hits: &mut Vec<LineAnswerHit>,
    current: &mut Vec<Span<'static>>,
) {
    if current.is_empty() {
        out.push(Line::default());
    } else {
        out.push(Line::from(std::mem::take(current)));
    }
    hits.push(None);
}

fn md_current_style(open: &[MdOpenTag], quote_depth: usize, link_url: Option<&str>) -> Style {
    let mut style = Style::default().fg(theme::text());
    if quote_depth > 0 {
        style = style.fg(theme::muted()).add_modifier(Modifier::ITALIC);
    }
    if open.iter().any(|t| matches!(t, MdOpenTag::Heading)) {
        style = style.fg(theme::assistant()).add_modifier(Modifier::BOLD);
    }
    if open.iter().any(|t| matches!(t, MdOpenTag::Strong)) {
        style = style.add_modifier(Modifier::BOLD);
    }
    if open.iter().any(|t| matches!(t, MdOpenTag::Emphasis)) {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if open.iter().any(|t| matches!(t, MdOpenTag::Strike)) {
        style = style.add_modifier(Modifier::CROSSED_OUT);
    }
    if link_url.is_some() {
        style = style
            .fg(theme::assistant())
            .add_modifier(Modifier::UNDERLINED);
    }
    style
}

#[derive(Debug, Default)]
struct MdTableState {
    alignments: Vec<Alignment>,
    in_head: bool,
    current_row: Vec<String>,
    current_cell: String,
    header_rows: Vec<Vec<String>>,
    body_rows: Vec<Vec<String>>,
}

fn align_table_cell(text: &str, width: usize, alignment: Alignment) -> String {
    let normalized = if text.chars().count() > width {
        truncate_chars(text, width)
    } else {
        text.to_string()
    };
    let cell_width = normalized.chars().count();
    if cell_width >= width {
        return normalized;
    }
    let pad = width - cell_width;
    match alignment {
        Alignment::Right => format!("{}{}", " ".repeat(pad), normalized),
        Alignment::Center => {
            let left = pad / 2;
            let right = pad - left;
            format!("{}{}{}", " ".repeat(left), normalized, " ".repeat(right))
        }
        _ => format!("{}{}", normalized, " ".repeat(pad)),
    }
}

fn render_table_lines(
    out: &mut Vec<Line<'static>>,
    hits: &mut Vec<LineAnswerHit>,
    table: &MdTableState,
    table_width: usize,
) {
    let col_count = table
        .header_rows
        .iter()
        .chain(table.body_rows.iter())
        .map(|r| r.len())
        .max()
        .unwrap_or(0);
    if col_count == 0 {
        return;
    }
    const ROW_LABEL: &str = " row   ";
    const COL_SEP: &str = "  ·  ";
    // Natural width = widest cell per column. Then fit columns to the
    // available transcript width: expand to content when there's room,
    // shrink proportionally (min 3) when the table would overflow.
    let mut col_widths = vec![0usize; col_count];
    for row in table.header_rows.iter().chain(table.body_rows.iter()) {
        for (cell, w) in row.iter().zip(col_widths.iter_mut()) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let chrome = ROW_LABEL.chars().count() + COL_SEP.chars().count() * col_count.saturating_sub(1);
    let cell_budget = table_width.saturating_sub(chrome).max(col_count * 3);
    let natural_total: usize = col_widths.iter().sum();
    if natural_total > cell_budget {
        for w in col_widths.iter_mut() {
            *w = ((*w * cell_budget) / natural_total.max(1)).max(3);
        }
    }
    for row in &table.header_rows {
        push_table_row(out, hits, row, true, &col_widths, &table.alignments);
    }
    if !table.header_rows.is_empty() {
        // Separator spans the actual rendered table width (label + columns + seps).
        let rule_width = ROW_LABEL.chars().count()
            + col_widths.iter().sum::<usize>()
            + COL_SEP.chars().count() * col_count.saturating_sub(1);
        out.push(Line::from(Span::styled(
            "─".repeat(rule_width.max(1)),
            Style::default().fg(theme::muted()),
        )));
        hits.push(None);
    }
    for row in &table.body_rows {
        push_table_row(out, hits, row, false, &col_widths, &table.alignments);
    }
}

const TABLE_ROW_LABEL: &str = " row   ";
const TABLE_COL_SEP: &str = "  ·  ";

/// Render one table row, wrapping long cells across multiple lines instead of
/// truncating. Continuation lines drop the row label and align under each
/// column.
fn push_table_row(
    out: &mut Vec<Line<'static>>,
    hits: &mut Vec<LineAnswerHit>,
    row: &[String],
    is_header: bool,
    col_widths: &[usize],
    alignments: &[Alignment],
) {
    let col_count = col_widths.len();
    let wrapped: Vec<Vec<String>> = col_widths
        .iter()
        .enumerate()
        .map(|(i, &w)| wrap_cell(row.get(i).map(String::as_str).unwrap_or(""), w.max(1)))
        .collect();
    let height = wrapped.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let label_style = Style::default()
        .fg(if is_header {
            theme::assistant()
        } else {
            theme::muted()
        })
        .add_modifier(Modifier::BOLD);
    let cell_style = if is_header {
        Style::default()
            .fg(theme::assistant())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::text())
    };
    let label_w = TABLE_ROW_LABEL.chars().count();
    for line_idx in 0..height {
        let mut spans = Vec::new();
        let label = if line_idx == 0 {
            if is_header {
                " table ".to_string()
            } else {
                TABLE_ROW_LABEL.to_string()
            }
        } else {
            " ".repeat(label_w)
        };
        spans.push(Span::styled(label, label_style));
        for (i, &cw) in col_widths.iter().enumerate() {
            let cell_line = wrapped[i].get(line_idx).map(String::as_str).unwrap_or("");
            let aligned = align_table_cell(
                cell_line,
                cw,
                alignments.get(i).copied().unwrap_or(Alignment::None),
            );
            spans.push(Span::styled(aligned, cell_style));
            if i + 1 != col_count {
                spans.push(Span::styled(
                    TABLE_COL_SEP,
                    Style::default().fg(theme::muted()),
                ));
            }
        }
        out.push(Line::from(spans));
        hits.push(None);
    }
}

/// Word-wrap a cell to `width`, hard-splitting words longer than the column.
fn wrap_cell(text: &str, width: usize) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let mut word = word;
        // Hard-split a single word that can't fit the column.
        while word.chars().count() > width {
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
            }
            let head: String = word.chars().take(width).collect();
            let consumed = head.len();
            lines.push(head);
            word = &word[consumed..];
        }
        let cur_len = cur.chars().count();
        if cur.is_empty() {
            cur.push_str(word);
        } else if cur_len + 1 + word.chars().count() <= width {
            cur.push(' ');
            cur.push_str(word);
        } else {
            lines.push(std::mem::take(&mut cur));
            cur.push_str(word);
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

pub(crate) fn render_markdown_lines_with_hits(
    markdown: &str,
    code_line_numbers: bool,
    table_width: usize,
) -> (Vec<Line<'static>>, Vec<LineAnswerHit>) {
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut hits: Vec<LineAnswerHit> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut open: Vec<MdOpenTag> = Vec::new();
    let mut list_stack: Vec<MdListState> = Vec::new();
    let mut quote_depth = 0usize;
    let mut pending_item_prefix: Option<String> = None;
    let mut active_link_url: Option<String> = None;
    let mut code_language: Option<String> = None;
    let mut code_buf = String::new();
    let mut table_state: Option<MdTableState> = None;

    let parser = Parser::new_ext(markdown, Options::all());

    for ev in parser {
        if let Some(table) = table_state.as_mut() {
            match ev {
                MdEvent::Start(Tag::TableHead) => table.in_head = true,
                MdEvent::Start(Tag::TableRow) => table.current_row.clear(),
                MdEvent::Start(Tag::TableCell) => table.current_cell.clear(),
                MdEvent::Text(t)
                | MdEvent::Code(t)
                | MdEvent::InlineMath(t)
                | MdEvent::DisplayMath(t) => table.current_cell.push_str(&t),
                MdEvent::SoftBreak | MdEvent::HardBreak => table.current_cell.push(' '),
                MdEvent::End(TagEnd::TableCell) => {
                    table
                        .current_row
                        .push(table.current_cell.trim().to_string());
                    table.current_cell.clear();
                }
                MdEvent::End(TagEnd::TableRow) => {
                    if table.in_head {
                        table.header_rows.push(table.current_row.clone());
                    } else {
                        table.body_rows.push(table.current_row.clone());
                    }
                    table.current_row.clear();
                }
                MdEvent::End(TagEnd::TableHead) => table.in_head = false,
                MdEvent::End(TagEnd::Table) => {
                    if !current.is_empty() {
                        flush_md_render_line(&mut out, &mut hits, &mut current);
                    }
                    if let Some(done) = table_state.take() {
                        render_table_lines(&mut out, &mut hits, &done, table_width);
                    }
                    out.push(Line::default());
                    hits.push(None);
                }
                _ => {}
            }
            continue;
        }

        if code_language.is_some() {
            match ev {
                MdEvent::Text(t)
                | MdEvent::Code(t)
                | MdEvent::InlineMath(t)
                | MdEvent::DisplayMath(t) => {
                    code_buf.push_str(&t);
                }
                MdEvent::SoftBreak | MdEvent::HardBreak => code_buf.push('\n'),
                MdEvent::End(_) => {
                    if matches!(open.pop(), Some(MdOpenTag::CodeBlock)) {
                        if !current.is_empty() {
                            flush_md_render_line(&mut out, &mut hits, &mut current);
                        }
                        render_code_block_lines(
                            &mut out,
                            &mut hits,
                            code_language.take(),
                            &code_buf,
                            code_line_numbers,
                        );
                        code_buf.clear();
                        out.push(Line::default());
                        hits.push(None);
                    }
                }
                _ => {}
            }
            continue;
        }

        match ev {
            MdEvent::Start(tag) => match tag {
                Tag::Strong => open.push(MdOpenTag::Strong),
                Tag::Emphasis => open.push(MdOpenTag::Emphasis),
                Tag::Strikethrough => open.push(MdOpenTag::Strike),
                Tag::Link { dest_url, .. } => {
                    active_link_url = Some(dest_url.to_string());
                    open.push(MdOpenTag::Link);
                }
                Tag::Paragraph => open.push(MdOpenTag::Paragraph),
                Tag::Heading { level, .. } => {
                    if !current.is_empty() {
                        flush_md_render_line(&mut out, &mut hits, &mut current);
                    }
                    let _ = level;
                    open.push(MdOpenTag::Heading);
                }
                Tag::BlockQuote(_) => {
                    quote_depth += 1;
                    open.push(MdOpenTag::BlockQuote);
                }
                Tag::List(start) => {
                    list_stack.push(match start {
                        Some(n) => MdListState::Ordered(n),
                        None => MdListState::Unordered,
                    });
                    open.push(MdOpenTag::List);
                }
                Tag::Item => {
                    if !current.is_empty() {
                        flush_md_render_line(&mut out, &mut hits, &mut current);
                    }
                    pending_item_prefix = Some(match list_stack.last_mut() {
                        Some(MdListState::Ordered(next)) => {
                            let label = format!("{}. ", *next);
                            *next += 1;
                            label
                        }
                        _ => "• ".to_string(),
                    });
                    open.push(MdOpenTag::Item);
                }
                Tag::CodeBlock(kind) => {
                    if !current.is_empty() {
                        flush_md_render_line(&mut out, &mut hits, &mut current);
                    }
                    code_language = Some(match kind {
                        CodeBlockKind::Indented => String::new(),
                        CodeBlockKind::Fenced(info) => info
                            .split_whitespace()
                            .next()
                            .unwrap_or_default()
                            .to_string(),
                    });
                    open.push(MdOpenTag::CodeBlock);
                }
                Tag::Table(alignments) => {
                    if !current.is_empty() {
                        flush_md_render_line(&mut out, &mut hits, &mut current);
                    }
                    table_state = Some(MdTableState {
                        alignments,
                        ..Default::default()
                    });
                }
                _ => open.push(MdOpenTag::Other),
            },
            MdEvent::End(_) => {
                let ended = open.pop();
                match ended {
                    Some(MdOpenTag::Paragraph) => {
                        if !current.is_empty() {
                            flush_md_render_line(&mut out, &mut hits, &mut current);
                        }
                        // Add extra vertical rhythm between top-level paragraphs.
                        if list_stack.is_empty() && quote_depth == 0 {
                            push_markdown_blank_line_if_needed(&mut out, &mut hits);
                        }
                    }
                    Some(MdOpenTag::Heading) | Some(MdOpenTag::Item) => {
                        if !current.is_empty() {
                            flush_md_render_line(&mut out, &mut hits, &mut current);
                        }
                    }
                    Some(MdOpenTag::BlockQuote) => {
                        quote_depth = quote_depth.saturating_sub(1);
                        if !current.is_empty() {
                            flush_md_render_line(&mut out, &mut hits, &mut current);
                        }
                    }
                    Some(MdOpenTag::Link) => {
                        active_link_url = None;
                    }
                    _ => {}
                }
            }
            MdEvent::Text(t) => {
                let style = md_current_style(&open, quote_depth, active_link_url.as_deref());
                let text = t.to_string();
                for (idx, seg) in text.split('\n').enumerate() {
                    if idx > 0 {
                        flush_md_render_line(&mut out, &mut hits, &mut current);
                    }
                    if current.is_empty() {
                        if quote_depth > 0 {
                            current.push(Span::styled(
                                "▎ ".repeat(quote_depth),
                                Style::default().fg(theme::warn()),
                            ));
                        }
                        if let Some(prefix) = pending_item_prefix.take() {
                            current.push(Span::styled(
                                prefix,
                                Style::default().fg(theme::assistant()),
                            ));
                        }
                    }
                    if !seg.is_empty() {
                        current.push(Span::styled(seg.to_string(), style));
                    }
                }
            }
            MdEvent::Code(code) => {
                let style = Style::default().fg(theme::tool()).bg(theme::surface());
                current.push(Span::styled(code.to_string(), style));
            }
            MdEvent::SoftBreak | MdEvent::HardBreak => {
                flush_md_render_line(&mut out, &mut hits, &mut current);
            }
            MdEvent::Rule => {
                if !current.is_empty() {
                    flush_md_render_line(&mut out, &mut hits, &mut current);
                }
                out.push(Line::from(Span::styled(
                    "────────────────",
                    Style::default().fg(theme::muted()),
                )));
                hits.push(None);
            }
            MdEvent::Html(html) | MdEvent::InlineHtml(html) => {
                current.push(Span::styled(
                    html.to_string(),
                    Style::default().fg(theme::muted()),
                ));
            }
            MdEvent::FootnoteReference(name) => {
                current.push(Span::styled(
                    format!("[^{}]", name),
                    Style::default().fg(theme::muted()),
                ));
            }
            MdEvent::TaskListMarker(done) => {
                let marker = if done { "[x] " } else { "[ ] " };
                current.push(Span::styled(
                    marker,
                    Style::default().fg(theme::assistant()),
                ));
            }
            _ => {}
        }
    }

    if !current.is_empty() {
        flush_md_render_line(&mut out, &mut hits, &mut current);
    }

    if out.is_empty() {
        (vec![Line::default()], vec![None])
    } else {
        (out, hits)
    }
}

#[cfg(test)]
pub(crate) fn render_markdown_lines(markdown: &str) -> Vec<Line<'static>> {
    render_markdown_lines_with_hits(markdown, false, 80).0
}

fn push_markdown_blank_line_if_needed(out: &mut Vec<Line<'static>>, hits: &mut Vec<LineAnswerHit>) {
    if out.last().is_some_and(|line| line.spans.is_empty()) {
        return;
    }
    out.push(Line::default());
    hits.push(None);
}

#[allow(dead_code)]
fn flush_md_plain(spans: &mut Vec<Span<'static>>, plain: &mut String, base: Style) {
    if !plain.is_empty() {
        spans.push(Span::styled(std::mem::take(plain), base));
    }
}

#[allow(dead_code)]
fn parse_md_inline(text: &str, base: Style) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut plain = String::new();
    let mut i = 0usize;

    while i < text.len() {
        let rest = &text[i..];

        if let Some(inner) = rest.strip_prefix("**")
            && let Some(end) = inner.find("**")
        {
            flush_md_plain(&mut spans, &mut plain, base);
            spans.push(Span::styled(
                inner[..end].to_string(),
                base.add_modifier(Modifier::BOLD),
            ));
            i += 2 + end + 2;
            continue;
        }

        if let Some(inner) = rest.strip_prefix("~~")
            && let Some(end) = inner.find("~~")
        {
            flush_md_plain(&mut spans, &mut plain, base);
            spans.push(Span::styled(
                inner[..end].to_string(),
                base.add_modifier(Modifier::CROSSED_OUT),
            ));
            i += 2 + end + 2;
            continue;
        }

        if let Some(inner) = rest.strip_prefix('`')
            && let Some(end) = inner.find('`')
        {
            flush_md_plain(&mut spans, &mut plain, base);
            spans.push(Span::styled(
                inner[..end].to_string(),
                base.fg(theme::tool()).bg(theme::surface()),
            ));
            i += 1 + end + 1;
            continue;
        }

        if let Some(inner) = rest.strip_prefix('*')
            && !rest.starts_with("**")
            && let Some(end) = inner.find('*')
        {
            flush_md_plain(&mut spans, &mut plain, base);
            spans.push(Span::styled(
                inner[..end].to_string(),
                base.add_modifier(Modifier::ITALIC),
            ));
            i += 1 + end + 1;
            continue;
        }

        if let Some(inner) = rest.strip_prefix('_')
            && !rest.starts_with("__")
            && let Some(end) = inner.find('_')
        {
            flush_md_plain(&mut spans, &mut plain, base);
            spans.push(Span::styled(
                inner[..end].to_string(),
                base.add_modifier(Modifier::ITALIC),
            ));
            i += 1 + end + 1;
            continue;
        }

        if let Some(inner) = rest.strip_prefix('[')
            && let Some(label_end) = inner.find("](")
        {
            let after = &inner[label_end + 2..];
            if let Some(url_end) = after.find(')') {
                flush_md_plain(&mut spans, &mut plain, base);
                let label = &inner[..label_end];
                let url = &after[..url_end];
                spans.push(Span::styled(
                    label.to_string(),
                    base.fg(theme::assistant())
                        .add_modifier(Modifier::UNDERLINED),
                ));
                if !url.is_empty() {
                    spans.push(Span::styled(
                        format!(" ({url})"),
                        base.fg(theme::muted()).add_modifier(Modifier::ITALIC),
                    ));
                }
                i += 1 + label_end + 2 + url_end + 1;
                continue;
            }
        }

        let mut iter = rest.char_indices();
        if let Some((_, ch)) = iter.next() {
            plain.push(ch);
            i += ch.len_utf8();
        } else {
            break;
        }
    }

    flush_md_plain(&mut spans, &mut plain, base);
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base));
    }
    spans
}

#[allow(dead_code)]
fn split_markdown_heading(text: &str) -> Option<(usize, &str)> {
    let level = text.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&level) && text.chars().nth(level) == Some(' ') {
        Some((level, &text[level + 1..]))
    } else {
        None
    }
}

#[allow(dead_code)]
pub(crate) fn parse_md_line(line: &str) -> Line<'static> {
    let base = Style::default().fg(theme::text());
    let trimmed = line.trim_start();
    let leading = &line[..line.len().saturating_sub(trimmed.len())];

    if trimmed.starts_with("```") {
        return Line::from(Span::styled(
            line.to_string(),
            Style::default().fg(theme::muted()),
        ));
    }

    if trimmed.len() >= 3 && trimmed.chars().all(|c| c == '-') {
        return Line::from(Span::styled(
            "─".repeat(trimmed.len()),
            Style::default().fg(theme::muted()),
        ));
    }

    if let Some((_heading_level, text)) = split_markdown_heading(trimmed) {
        let mut spans = Vec::new();
        if !leading.is_empty() {
            spans.push(Span::styled(leading.to_string(), base));
        }
        spans.extend(parse_md_inline(
            text,
            Style::default()
                .fg(theme::assistant())
                .add_modifier(Modifier::BOLD),
        ));
        return Line::from(spans);
    }

    if let Some(text) = trimmed.strip_prefix("> ") {
        let mut spans = Vec::new();
        if !leading.is_empty() {
            spans.push(Span::styled(leading.to_string(), base));
        }
        spans.push(Span::styled("▎ ", Style::default().fg(theme::warn())));
        let quoted = if let Some((_level, content)) = split_markdown_heading(text) {
            content
        } else {
            text
        };
        spans.extend(parse_md_inline(
            quoted,
            Style::default()
                .fg(theme::muted())
                .add_modifier(Modifier::ITALIC),
        ));
        return Line::from(spans);
    }

    if let Some(text) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
    {
        let mut spans = Vec::new();
        if !leading.is_empty() {
            spans.push(Span::styled(leading.to_string(), base));
        }
        spans.push(Span::styled("• ", Style::default().fg(theme::assistant())));
        spans.extend(parse_md_inline(text, base));
        return Line::from(spans);
    }

    if let Some(dot) = trimmed.find(". ")
        && !trimmed[..dot].is_empty()
        && trimmed[..dot].chars().all(|c| c.is_ascii_digit())
    {
        let mut spans = Vec::new();
        if !leading.is_empty() {
            spans.push(Span::styled(leading.to_string(), base));
        }
        spans.push(Span::styled(
            format!("{}. ", &trimmed[..dot]),
            Style::default().fg(theme::assistant()),
        ));
        spans.extend(parse_md_inline(&trimmed[dot + 2..], base));
        return Line::from(spans);
    }

    Line::from(parse_md_inline(line, base))
}
