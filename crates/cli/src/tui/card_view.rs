//! A board card in full (Enter on the board): a header with its key, type,
//! status and priority; a grid of who/when; then sections — what someone
//! else did, the ticket's fields, the description (light markdown), links,
//! attachments, comments, the workflow moves it can make, what's waiting to
//! go to Jira, and the card's history. Lines are laid out to `width` here,
//! so the grid lines up.

use dcode_ai_common::board::{Board, Card, Priority, Status};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::tui::theme;

/// Words wrapped to `w` columns (long words are cut).
pub fn wrap(text: &str, w: usize) -> Vec<String> {
    let w = w.max(8);
    let mut out = Vec::new();
    for para in text.split('\n') {
        let mut cur = String::new();
        for word in para.split_whitespace() {
            let mut word = word.to_string();
            while word.chars().count() > w {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                let head: String = word.chars().take(w).collect();
                word = word.chars().skip(w).collect();
                out.push(head);
            }
            if cur.is_empty() {
                cur = word;
            } else if cur.chars().count() + 1 + word.chars().count() <= w {
                cur.push(' ');
                cur.push_str(&word);
            } else {
                out.push(std::mem::replace(&mut cur, word));
            }
        }
        out.push(cur);
    }
    out
}

fn fg(c: Color) -> Style {
    Style::default().fg(c)
}

fn pad(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n >= w {
        s.chars().take(w).collect()
    } else {
        format!("{s}{}", " ".repeat(w - n))
    }
}

fn cut(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(w.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

pub fn status_color(s: Status) -> Color {
    match s {
        Status::Backlog => theme::muted(),
        Status::Todo => theme::text(),
        Status::InProgress => theme::accent(),
        Status::Review => theme::warn(),
        Status::Done => theme::success(),
    }
}

fn priority_label(p: Priority) -> (&'static str, Color) {
    match p {
        Priority::Urgent => ("⏫ Urgent", theme::error()),
        Priority::High => ("▲ High", theme::warn()),
        Priority::Medium => ("■ Medium", theme::muted()),
        Priority::Low => ("▽ Low", theme::muted()),
    }
}

/// A pill: ` text ` on a tinted background.
fn chip(text: &str, color: Color) -> Span<'static> {
    Span::styled(
        format!(" {text} "),
        Style::default()
            .fg(color)
            .add_modifier(Modifier::BOLD | Modifier::REVERSED),
    )
}

/// `── Title ───────────` across the width.
fn section(title: &str, w: usize) -> Line<'static> {
    let head = format!("── {title} ");
    let rest = w.saturating_sub(head.chars().count());
    Line::from(vec![
        Span::styled(
            head,
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("─".repeat(rest), fg(theme::border())),
    ])
}

/// Field values that are Jira plumbing, not something to read.
fn noise(name: &str, value: &str) -> bool {
    name.starts_with('[') || value.contains("_*:*") || value.trim().is_empty()
}

/// `label  value` pairs laid out in two columns when there's room.
fn grid(pairs: &[(String, String)], w: usize, out: &mut Vec<Line<'static>>) {
    let label_w = pairs
        .iter()
        .map(|(l, _)| l.chars().count())
        .max()
        .unwrap_or(0)
        .min(18)
        + 2;
    let two = w >= 76 && pairs.len() > 3;
    let col_w = if two { (w - 2) / 2 } else { w };
    let cell = |(l, v): &(String, String)| -> Vec<Span<'static>> {
        vec![
            Span::styled(pad(l, label_w), fg(theme::muted())),
            Span::styled(
                pad(
                    &cut(v, col_w.saturating_sub(label_w)),
                    col_w.saturating_sub(label_w),
                ),
                fg(theme::text()),
            ),
        ]
    };
    if two {
        let half = pairs.len().div_ceil(2);
        for i in 0..half {
            let mut spans = cell(&pairs[i]);
            if let Some(p) = pairs.get(i + half) {
                spans.push(Span::raw("  "));
                spans.extend(cell(p));
            }
            out.push(Line::from(spans));
        }
    } else {
        for p in pairs {
            out.push(Line::from(cell(p)));
        }
    }
}

/// Markdown-ish text: `#` headings bold, `-`/`*` bullets as `•`, `>` quotes
/// muted, the rest wrapped.
fn rich_text(text: &str, w: usize, out: &mut Vec<Line<'static>>) {
    let mut blank = false;
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.trim().is_empty() {
            if !blank && !out.is_empty() {
                out.push(Line::default());
            }
            blank = true;
            continue;
        }
        blank = false;
        let t = line.trim_start();
        if let Some(h) = t.strip_prefix('#') {
            let h = h.trim_start_matches('#').trim();
            out.push(Line::from(Span::styled(
                h.to_string(),
                Style::default()
                    .fg(theme::text())
                    .add_modifier(Modifier::BOLD),
            )));
        } else if let Some(item) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            for (i, row) in wrap(item, w.saturating_sub(3)).into_iter().enumerate() {
                let lead = if i == 0 { " • " } else { "   " };
                out.push(Line::from(vec![
                    Span::styled(lead, fg(theme::accent())),
                    Span::styled(row, fg(theme::text())),
                ]));
            }
        } else if let Some(q) = t.strip_prefix('>') {
            for row in wrap(q.trim(), w.saturating_sub(2)) {
                out.push(Line::from(vec![
                    Span::styled("▎ ", fg(theme::border())),
                    Span::styled(row, fg(theme::muted())),
                ]));
            }
        } else {
            for row in wrap(t, w) {
                out.push(Line::from(Span::styled(row, fg(theme::text()))));
            }
        }
    }
    while out.last().is_some_and(|l| l.spans.is_empty()) {
        out.pop();
    }
}

fn day(s: &str) -> String {
    s.get(..10).unwrap_or(s).to_string()
}

/// The card in full, `width` columns wide.
pub fn card_view_lines(board: &Board, c: &Card, width: usize) -> Vec<Line<'static>> {
    let w = width.max(30);
    let mut out: Vec<Line<'static>> = Vec::new();
    let rm = c.remote.as_ref();

    // ── Header: key · type, then status and priority on the right.
    let jira_status = rm.map(|r| r.status.clone()).filter(|s| !s.is_empty());
    let status_text = match &jira_status {
        Some(s) if !s.eq_ignore_ascii_case(c.status.label()) => {
            format!("{} · {s}", c.status.label())
        }
        _ => c.status.label().to_string(),
    };
    let (prio, prio_color) = priority_label(c.priority);
    let mut left = vec![Span::styled(
        c.shown_key().to_string(),
        Style::default()
            .fg(theme::accent())
            .add_modifier(Modifier::BOLD),
    )];
    if let Some(t) = rm.map(|r| r.issue_type.as_str()).filter(|t| !t.is_empty()) {
        left.push(Span::styled(format!("  {t}"), fg(theme::muted())));
    }
    if c.gate {
        left.push(Span::styled("  ◆ gate", fg(theme::warn())));
    }
    let right = [
        chip(&status_text, status_color(c.status)),
        Span::raw(" "),
        chip(prio, prio_color),
    ];
    let used: usize = left
        .iter()
        .chain(right.iter())
        .map(|s| s.content.chars().count())
        .sum();
    left.push(Span::raw(" ".repeat(w.saturating_sub(used))));
    left.extend(right);
    out.push(Line::from(left));
    out.push(Line::default());
    for row in wrap(&c.title, w) {
        out.push(Line::from(Span::styled(
            row,
            Style::default()
                .fg(theme::text())
                .add_modifier(Modifier::BOLD),
        )));
    }
    out.push(Line::default());

    // ── Who and when.
    let mut facts: Vec<(String, String)> = Vec::new();
    let mut fact = |l: &str, v: String| {
        if !v.trim().is_empty() {
            facts.push((l.to_string(), v));
        }
    };
    fact("Assignee", c.assignee.clone().unwrap_or_else(|| "—".into()));
    if let Some(r) = rm {
        fact("Reporter", r.reporter.clone());
        fact("Created", day(&r.created));
        fact("Updated", day(&r.updated));
        fact("Due", day(&r.due_date));
        fact("Parent", r.parent.clone());
        if !r.labels.is_empty() {
            fact("Labels", r.labels.join(", "));
        }
    } else {
        fact("Created", c.created.format("%Y-%m-%d").to_string());
        fact("Updated", c.updated.format("%Y-%m-%d").to_string());
    }
    fact("Branch", c.branch.clone().unwrap_or_default());
    if let Some(m) = c
        .milestone
        .as_deref()
        .and_then(|m| board.milestone_title(m))
    {
        fact("Milestone", m.to_string());
    }
    if !c.blocked_by.is_empty() {
        fact("Blocked by", c.blocked_by.join(", "));
    }
    fact("Space", c.space.label().to_string());
    grid(&facts, w, &mut out);

    if let Some(act) = rm.map(|r| r.activity.as_str()).filter(|a| !a.is_empty()) {
        out.push(Line::default());
        out.push(Line::from(vec![
            Span::styled(
                "● ",
                Style::default()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                act.to_string(),
                Style::default()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    // ── The ticket's own fields.
    if let Some(r) = rm {
        let fields: Vec<(String, String)> = r
            .fields
            .iter()
            .filter(|(k, v)| !noise(k, v))
            .cloned()
            .collect();
        if !fields.is_empty() {
            out.push(Line::default());
            out.push(section("Details", w));
            grid(&fields, w, &mut out);
        }
    }

    if !c.description.trim().is_empty() {
        out.push(Line::default());
        out.push(section("Description", w));
        // A description often opens with its own "Description" heading.
        let body = c.description.trim();
        let body = body
            .strip_prefix("## Description")
            .or_else(|| body.strip_prefix("# Description"))
            .unwrap_or(body);
        rich_text(body, w, &mut out);
    }

    if let Some(r) = rm {
        if !r.links.is_empty() {
            out.push(Line::default());
            out.push(section("Linked", w));
            for l in &r.links {
                out.push(Line::from(vec![
                    Span::styled(" ↳ ", fg(theme::accent())),
                    Span::styled(cut(l, w.saturating_sub(3)), fg(theme::text())),
                ]));
            }
        }
        if !r.attachments.is_empty() {
            out.push(Line::default());
            out.push(section(
                &format!("Attachments ({}) · a downloads", r.attachments.len()),
                w,
            ));
            for (i, a) in r.attachments.iter().enumerate() {
                let size = match a.size {
                    0 => String::new(),
                    n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
                    n => format!("{} KB", n.div_ceil(1024)),
                };
                let waiting = r.downloads.contains(&a.id);
                out.push(Line::from(vec![
                    Span::styled(format!(" {} ", i + 1), fg(theme::accent())),
                    Span::styled(cut(&a.filename, w.saturating_sub(16)), fg(theme::text())),
                    Span::styled(format!("  {size}"), fg(theme::muted())),
                    Span::styled(
                        if waiting { "  ⟳ downloading" } else { "" },
                        fg(theme::warn()),
                    ),
                ]));
            }
        }
        if !r.comments.is_empty() {
            out.push(Line::default());
            let total = r.comment_count.max(r.comments.len() as u32);
            out.push(section(
                &format!("Comments ({total}) · c comment · R reply"),
                w,
            ));
            for (i, cm) in r.comments.iter().enumerate() {
                if i > 0 {
                    out.push(Line::default());
                }
                out.push(Line::from(vec![
                    Span::styled(
                        cm.author.clone(),
                        Style::default()
                            .fg(theme::text())
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  {}", cm.at.get(..16).unwrap_or(&cm.at).replace('T', " ")),
                        fg(theme::muted()),
                    ),
                ]));
                let mut body = Vec::new();
                rich_text(&cm.body, w.saturating_sub(2), &mut body);
                for l in body.into_iter().take(12) {
                    let mut spans = vec![Span::styled("  ", Style::default())];
                    spans.extend(l.spans);
                    out.push(Line::from(spans));
                }
            }
        }
        if !r.transitions.is_empty() {
            out.push(Line::default());
            out.push(section("Move · t picks", w));
            let mut row: Vec<Span<'static>> = Vec::new();
            let mut used = 0;
            for (i, t) in r.transitions.iter().enumerate() {
                let label = format!("{} {}", i + 1, t.label());
                let n = label.chars().count() + 3;
                if used + n > w && !row.is_empty() {
                    out.push(Line::from(std::mem::take(&mut row)));
                    used = 0;
                }
                row.push(chip(&label, theme::accent()));
                row.push(Span::raw(" "));
                used += n;
            }
            if !row.is_empty() {
                out.push(Line::from(row));
            }
        }
        // Waiting to go to Jira.
        let mut waiting: Vec<String> = Vec::new();
        if !r.want_transition.is_empty() {
            waiting.push(format!("move: {}", r.want_transition));
        }
        for o in &r.outbox {
            waiting.push(format!(
                "{}: {}",
                if o.internal {
                    "internal note"
                } else {
                    "comment"
                },
                cut(&o.body, 80)
            ));
        }
        for wl in &r.worklogs {
            waiting.push(format!("log {} {}", wl.time, wl.comment).trim().to_string());
        }
        if !waiting.is_empty() {
            out.push(Line::default());
            out.push(section("Waiting to sync", w));
            for x in waiting {
                out.push(Line::from(vec![
                    Span::styled(" ⟳ ", fg(theme::warn())),
                    Span::styled(cut(&x, w.saturating_sub(3)), fg(theme::text())),
                ]));
            }
        }
    }

    if !c.notes.is_empty() {
        out.push(Line::default());
        out.push(section("History", w));
        for n in c.notes.iter().rev().take(12) {
            let who = Span::styled(
                pad(&n.by, 6),
                fg(if n.by == "jira" {
                    theme::accent()
                } else {
                    theme::muted()
                }),
            );
            let when =
                n.at.with_timezone(&chrono::Local)
                    .format("%m-%d %H:%M  ")
                    .to_string();
            let rows = wrap(&n.text, w.saturating_sub(20));
            for (i, row) in rows.into_iter().enumerate() {
                if i == 0 {
                    out.push(Line::from(vec![
                        Span::styled(when.clone(), fg(theme::muted())),
                        who.clone(),
                        Span::styled(row, fg(theme::text())),
                    ]));
                } else {
                    out.push(Line::from(vec![
                        Span::raw(" ".repeat(19)),
                        Span::styled(row, fg(theme::text())),
                    ]));
                }
            }
        }
    }
    out
}

/// The key hints under a card: keys bright, words dim.
pub fn hints(jira: bool, w: usize) -> Line<'static> {
    let mut keys: Vec<(&str, &str)> = vec![("←→", "status")];
    if jira {
        keys.extend([
            ("t", "move"),
            ("c", "comment"),
            ("R", "reply"),
            ("C", "note"),
            ("w", "log"),
            ("a", "files"),
            ("o", "open"),
        ]);
    }
    keys.extend([
        ("e", "edit"),
        ("u", "assign"),
        ("p", "priority"),
        ("↑↓", "scroll"),
        ("Esc", "close"),
    ]);
    let mut spans = Vec::new();
    let mut used = 0;
    for (k, label) in keys {
        let n = k.chars().count() + label.chars().count() + 3;
        if used + n > w {
            break;
        }
        spans.push(Span::styled(
            k.to_string(),
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {label}  "), fg(theme::muted())));
        used += n;
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dcode_ai_common::board::{NewCard, Remote, RemoteComment, Space};

    fn text(lines: &[Line<'_>]) -> String {
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
    fn a_jira_card_reads_as_sections() {
        let mut b = Board::new("BSI");
        let k = b.add(
            NewCard {
                title: "Super critical : zeocircle+sp".into(),
                description: "## Description\n\nTeam We need to explore options.\n\n- Copilot agent + crm\n- Zeocirl + sp".into(),
                space: Space::Jira,
                ..Default::default()
            },
            "you",
        );
        let c = b.card_mut(&k).unwrap();
        c.remote = Some(Remote {
            key: "BSI-524".into(),
            status: "Resolved".into(),
            issue_type: "Task".into(),
            reporter: "Sreelekshmi Latha".into(),
            created: "2026-10-22T06:27:20".into(),
            fields: vec![
                ("Task Type".into(), "Build and Development".into()),
                ("[CHART] Time in Status".into(), "1_*:*_1_*:*_113".into()),
            ],
            comments: vec![RemoteComment {
                author: "Mamatha Suresh".into(),
                at: "2026-11-04T09:00:00".into(),
                body: "Any update?".into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        c.push = None;
        let c = c.clone();
        let lines = card_view_lines(&b, &c, 90);
        let t = text(&lines);
        assert!(t.starts_with("BSI-524  Task"), "{t}");
        assert!(t.contains(" Resolved "), "{t}");
        assert!(t.contains("── Details"), "{t}");
        assert!(t.contains("Task Type"));
        assert!(!t.contains("_*:*"), "chart plumbing is hidden: {t}");
        assert!(!t.contains("## Description"), "{t}");
        assert!(t.contains(" • Copilot agent + crm"), "{t}");
        assert!(t.contains("── Comments (1)"));
        assert!(t.contains("Mamatha Suresh  2026-11-04 09:00"));
        // Nothing runs past the width.
        assert!(lines.iter().all(|l| l.width() <= 90), "{t}");
    }

    #[test]
    fn long_words_wrap() {
        assert_eq!(wrap("aaaaaaaaaaaa bb", 8), vec!["aaaaaaaa", "aaaa bb"]);
    }
}
