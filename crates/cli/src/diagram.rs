//! Mermaid diagrams drawn in the terminal: `flowchart`/`graph` as boxes and
//! arrows (a layered layout: ranks by longest path, order in a rank by the
//! barycenter of neighbours, long edges through dummy points) and
//! `sequenceDiagram` as lifelines and messages. Other kinds: `None` (the
//! viewer shows the source).

use std::collections::HashMap;

/// The ```mermaid blocks in a reply.
pub fn extract(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    for line in text.lines() {
        let t = line.trim();
        match &mut cur {
            None if t.starts_with("```")
                && t.trim_start_matches('`')
                    .trim()
                    .eq_ignore_ascii_case("mermaid") =>
            {
                cur = Some(String::new())
            }
            Some(buf) if t.starts_with("```") => {
                out.push(std::mem::take(buf));
                cur = None;
            }
            Some(buf) => {
                buf.push_str(line);
                buf.push('\n');
            }
            None => {}
        }
    }
    out
}

/// Draws a diagram; `Err` says why it can't (unsupported kind, nothing in it).
pub fn render(src: &str) -> Result<Vec<String>, String> {
    let first = src
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("%%"))
        .unwrap_or("");
    let kind = first.split_whitespace().next().unwrap_or("");
    match kind {
        "graph" | "flowchart" => flowchart(src),
        "sequenceDiagram" => sequence(src),
        "" => Err("empty diagram".into()),
        k => Err(format!(
            "`{k}` diagrams aren't drawn yet — here is the source"
        )),
    }
}

// ── Canvas ─────────────────────────────────────────────────────────────

const UP: u8 = 1;
const DOWN: u8 = 2;
const LEFT: u8 = 4;
const RIGHT: u8 = 8;

#[derive(Clone, Copy, PartialEq)]
enum Stroke {
    Solid,
    Dotted,
    Thick,
}

#[derive(Clone, Copy)]
struct Cell {
    ch: char,
    mask: u8,
    stroke: Stroke,
}

struct Canvas {
    w: usize,
    h: usize,
    cells: Vec<Cell>,
}

impl Canvas {
    fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            cells: vec![
                Cell {
                    ch: ' ',
                    mask: 0,
                    stroke: Stroke::Solid
                };
                w * h
            ],
        }
    }
    fn get(&mut self, x: usize, y: usize) -> Option<&mut Cell> {
        (x < self.w && y < self.h).then(|| &mut self.cells[y * self.w + x])
    }
    fn put(&mut self, x: usize, y: usize, ch: char) {
        if let Some(c) = self.get(x, y) {
            c.ch = ch;
        }
    }
    /// Writes text only over empty cells (labels never cut a line or box).
    fn label(&mut self, x: usize, y: usize, text: &str) {
        let fits = text.chars().enumerate().all(|(i, _)| {
            self.get(x + i, y)
                .is_some_and(|c| c.ch == ' ' && c.mask == 0)
        });
        if fits {
            for (i, ch) in text.chars().enumerate() {
                self.put(x + i, y, ch);
            }
        }
    }
    fn text(&mut self, x: usize, y: usize, text: &str) {
        for (i, ch) in text.chars().enumerate() {
            self.put(x + i, y, ch);
        }
    }
    fn mark(&mut self, x: usize, y: usize, m: u8, stroke: Stroke) {
        if let Some(c) = self.get(x, y)
            && c.ch == ' '
        {
            c.mask |= m;
            if stroke != Stroke::Solid {
                c.stroke = stroke;
            }
        }
    }
    fn vline(&mut self, x: usize, y0: usize, y1: usize, s: Stroke) {
        let (a, b) = if y0 <= y1 { (y0, y1) } else { (y1, y0) };
        for y in a..=b {
            let mut m = 0;
            if y > a {
                m |= UP;
            }
            if y < b {
                m |= DOWN;
            }
            self.mark(x, y, m, s);
        }
    }
    fn hline(&mut self, y: usize, x0: usize, x1: usize, s: Stroke) {
        let (a, b) = if x0 <= x1 { (x0, x1) } else { (x1, x0) };
        for x in a..=b {
            let mut m = 0;
            if x > a {
                m |= LEFT;
            }
            if x < b {
                m |= RIGHT;
            }
            self.mark(x, y, m, s);
        }
    }
    fn boxed(&mut self, x: usize, y: usize, w: usize, h: usize, shape: Shape, lines: &[String]) {
        let (tl, tr, bl, br, hz, vt) = match shape {
            Shape::Round => ('╭', '╮', '╰', '╯', '─', '│'),
            Shape::Decision => ('╔', '╗', '╚', '╝', '═', '║'),
            Shape::Square => ('┌', '┐', '└', '┘', '─', '│'),
        };
        for i in 1..w.saturating_sub(1) {
            self.put(x + i, y, hz);
            self.put(x + i, y + h - 1, hz);
        }
        for j in 1..h.saturating_sub(1) {
            self.put(x, y + j, vt);
            self.put(x + w - 1, y + j, vt);
            for i in 1..w - 1 {
                self.put(x + i, y + j, '\u{a0}');
            }
        }
        self.put(x, y, tl);
        self.put(x + w - 1, y, tr);
        self.put(x, y + h - 1, bl);
        self.put(x + w - 1, y + h - 1, br);
        for (j, l) in lines.iter().enumerate() {
            let pad = (w - 2 - l.chars().count()) / 2;
            self.text(x + 1 + pad, y + 1 + j, l);
        }
    }
    fn lines(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.h);
        for y in 0..self.h {
            let mut s = String::with_capacity(self.w);
            for x in 0..self.w {
                let c = self.cells[y * self.w + x];
                let ch = if c.ch != ' ' {
                    if c.ch == '\u{a0}' { ' ' } else { c.ch }
                } else {
                    line_char(c.mask, c.stroke)
                };
                s.push(ch);
            }
            out.push(s.trim_end().to_string());
        }
        while out.last().is_some_and(|l| l.is_empty()) {
            out.pop();
        }
        out
    }
}

fn line_char(m: u8, s: Stroke) -> char {
    let v = m & (UP | DOWN) != 0;
    let h = m & (LEFT | RIGHT) != 0;
    match (s, v, h) {
        (Stroke::Dotted, true, false) => return '┊',
        (Stroke::Dotted, false, true) => return '┄',
        (Stroke::Thick, true, false) => return '┃',
        (Stroke::Thick, false, true) => return '━',
        _ => {}
    }
    match m {
        0 => ' ',
        x if x == UP | DOWN || x == UP || x == DOWN => '│',
        x if x == LEFT | RIGHT || x == LEFT || x == RIGHT => '─',
        x if x == DOWN | RIGHT => '┌',
        x if x == DOWN | LEFT => '┐',
        x if x == UP | RIGHT => '└',
        x if x == UP | LEFT => '┘',
        x if x == UP | DOWN | RIGHT => '├',
        x if x == UP | DOWN | LEFT => '┤',
        x if x == LEFT | RIGHT | DOWN => '┬',
        x if x == LEFT | RIGHT | UP => '┴',
        _ => '┼',
    }
}

// ── Flowchart ──────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Debug)]
enum Shape {
    Square,
    Round,
    Decision,
}

#[derive(Debug)]
struct Node {
    label: String,
    shape: Shape,
}

#[derive(Debug, Clone)]
struct Edge {
    from: usize,
    to: usize,
    label: String,
    stroke: Stroke,
    head: char,
    tail: bool,
}

impl std::fmt::Debug for Stroke {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Stroke::Solid => "solid",
            Stroke::Dotted => "dotted",
            Stroke::Thick => "thick",
        })
    }
}

#[derive(Default)]
struct Graph {
    nodes: Vec<Node>,
    ids: HashMap<String, usize>,
    edges: Vec<Edge>,
}

impl Graph {
    fn node(&mut self, id: &str, label: Option<(String, Shape)>) -> usize {
        if let Some(&i) = self.ids.get(id) {
            if let Some((l, s)) = label {
                self.nodes[i] = Node { label: l, shape: s };
            }
            return i;
        }
        let (l, s) = label.unwrap_or_else(|| (id.to_string(), Shape::Square));
        self.nodes.push(Node { label: l, shape: s });
        self.ids.insert(id.to_string(), self.nodes.len() - 1);
        self.nodes.len() - 1
    }
}

fn clean_label(s: &str) -> String {
    let s = s.trim().trim_matches('"').trim();
    s.replace("<br>", " ")
        .replace("<br/>", " ")
        .replace("<br />", " ")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

/// A node's label and shape, when the text gives them.
type NodeLabel = Option<(String, Shape)>;

/// One node at the start of `s`: (id, label+shape, rest).
fn parse_node(s: &str) -> Option<(String, NodeLabel, &str)> {
    let s = s.trim_start();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        let ok = c.is_alphanumeric()
            || c == '_'
            || (c == '-' && i + 1 < b.len() && (b[i + 1] as char).is_alphanumeric() && i > 0)
            || !c.is_ascii();
        if !ok {
            break;
        }
        i += c.len_utf8();
    }
    if i == 0 {
        return None;
    }
    let id = s[..i].to_string();
    let rest = &s[i..];
    const SHAPES: [(&str, &str, Shape); 9] = [
        ("((", "))", Shape::Round),
        ("([", "])", Shape::Round),
        ("[[", "]]", Shape::Square),
        ("[(", ")]", Shape::Round),
        ("{{", "}}", Shape::Decision),
        ("[", "]", Shape::Square),
        ("(", ")", Shape::Round),
        ("{", "}", Shape::Decision),
        (">", "]", Shape::Square),
    ];
    for (open, close, shape) in SHAPES {
        if let Some(after) = rest.strip_prefix(open) {
            // A quoted label may contain the closing bracket.
            let end = if after.trim_start().starts_with('"') {
                let q = after.find('"')?;
                let q2 = after[q + 1..].find('"')? + q + 1;
                after[q2..].find(close)? + q2
            } else {
                after.find(close)?
            };
            let label = clean_label(&after[..end]);
            let mut rest = &after[end + close.len()..];
            if let Some(r) = rest.strip_prefix(":::") {
                rest = r.trim_start_matches(|c: char| c.is_alphanumeric() || c == '_' || c == '-');
            }
            return Some((id, Some((label, shape)), rest));
        }
    }
    let mut rest = rest;
    if let Some(r) = rest.strip_prefix(":::") {
        rest = r.trim_start_matches(|c: char| c.is_alphanumeric() || c == '_' || c == '-');
    }
    Some((id, None, rest))
}

/// An edge at the start of `s`: (label, stroke, head, tail, rest).
fn parse_edge(s: &str) -> Option<(String, Stroke, char, bool, &str)> {
    let s = s.trim_start();
    if !(s.starts_with('-') || s.starts_with('=') || s.starts_with('<') || s.starts_with('~')) {
        return None;
    }
    let mut arrow_end = s
        .char_indices()
        .find(|(_, c)| !matches!(c, '-' | '=' | '.' | '<' | '>' | '~'))
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    let mut arrow = s[..arrow_end].to_string();
    // `--x` / `--o` heads, when followed by a space or the label.
    if let Some(c) = s[arrow_end..].chars().next()
        && (c == 'x' || c == 'o')
        && !arrow.ends_with('>')
        && s[arrow_end + 1..]
            .chars()
            .next()
            .is_none_or(|n| n.is_whitespace() || n == '|')
    {
        arrow.push(c);
        arrow_end += 1;
    }
    let mut rest = &s[arrow_end..];
    let mut label = String::new();
    // `-- text -->`, `== text ==>`, `-. text .->`.
    if matches!(arrow.as_str(), "--" | "==" | "-.") {
        let closers = ["-->", "---", "==>", "===", ".->", ".-", "--x", "--o"];
        let found = closers
            .iter()
            .filter_map(|c| rest.find(c).map(|i| (i, *c)))
            .min_by_key(|(i, _)| *i)?;
        label = clean_label(&rest[..found.0]);
        let mut full = arrow.clone();
        full.push_str(found.1);
        arrow = full;
        rest = &rest[found.0 + found.1.len()..];
    }
    let rest_trim = rest.trim_start();
    if let Some(r) = rest_trim.strip_prefix('|')
        && let Some(end) = r.find('|')
    {
        label = clean_label(&r[..end]);
        rest = &r[end + 1..];
    }
    let stroke = if arrow.contains('.') {
        Stroke::Dotted
    } else if arrow.contains('=') {
        Stroke::Thick
    } else {
        Stroke::Solid
    };
    let head = if arrow.ends_with('>') {
        '▼'
    } else if arrow.ends_with('x') {
        '✕'
    } else if arrow.ends_with('o') {
        '○'
    } else {
        ' '
    };
    Some((label, stroke, head, arrow.starts_with('<'), rest))
}

fn parse_flowchart(src: &str) -> (Graph, bool) {
    let mut g = Graph::default();
    let mut lr = false;
    let mut header = true;
    for raw in src.lines() {
        let line = raw.split("%%").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if header {
            header = false;
            let dir = line.split_whitespace().nth(1).unwrap_or("TD");
            lr = matches!(dir, "LR" | "RL");
            continue;
        }
        for stmt in line.split(';') {
            let stmt = stmt.trim();
            let kw = stmt.split_whitespace().next().unwrap_or("");
            if stmt.is_empty()
                || matches!(
                    kw,
                    "subgraph"
                        | "end"
                        | "style"
                        | "classDef"
                        | "class"
                        | "click"
                        | "linkStyle"
                        | "direction"
                )
            {
                continue;
            }
            parse_statement(&mut g, stmt);
        }
    }
    (g, lr)
}

fn parse_group<'a>(g: &mut Graph, s: &'a str) -> Option<(Vec<usize>, &'a str)> {
    let mut ids = Vec::new();
    let (id, label, mut rest) = parse_node(s)?;
    ids.push(g.node(&id, label));
    while let Some(r) = rest.trim_start().strip_prefix('&') {
        let (id, label, r2) = parse_node(r)?;
        ids.push(g.node(&id, label));
        rest = r2;
    }
    Some((ids, rest))
}

fn parse_statement(g: &mut Graph, stmt: &str) {
    let Some((mut from, mut rest)) = parse_group(g, stmt) else {
        return;
    };
    while let Some((label, stroke, head, tail, r)) = parse_edge(rest) {
        let Some((to, r2)) = parse_group(g, r) else {
            return;
        };
        for &a in &from {
            for &b in &to {
                g.edges.push(Edge {
                    from: a,
                    to: b,
                    label: label.clone(),
                    stroke,
                    head,
                    tail,
                });
            }
        }
        from = to;
        rest = r2;
    }
}

fn wrap_label(s: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = textwrap::wrap(s, width)
        .into_iter()
        .map(|c| c.into_owned())
        .collect();
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// A point in the layout: a node, or a dummy on a long edge.
struct Item {
    node: Option<usize>,
    w: usize,
    h: usize,
    lines: Vec<String>,
    x: usize,
    y: usize,
}

fn flowchart(src: &str) -> Result<Vec<String>, String> {
    let (g, lr) = parse_flowchart(src);
    if g.nodes.is_empty() {
        return Err("no nodes in the diagram".into());
    }
    let n = g.nodes.len();
    // Cycles: edges back to a node on the DFS stack are drawn as a list.
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, e) in g.edges.iter().enumerate() {
        if e.from != e.to {
            adj[e.from].push(i);
        }
    }
    let mut state = vec![0u8; n];
    let mut back = vec![false; g.edges.len()];
    fn dfs(v: usize, g: &Graph, adj: &[Vec<usize>], st: &mut [u8], back: &mut [bool]) {
        st[v] = 1;
        for &ei in &adj[v] {
            let to = g.edges[ei].to;
            match st[to] {
                0 => dfs(to, g, adj, st, back),
                1 => back[ei] = true,
                _ => {}
            }
        }
        st[v] = 2;
    }
    for v in 0..n {
        if state[v] == 0 {
            dfs(v, &g, &adj, &mut state, &mut back);
        }
    }
    let forward: Vec<&Edge> = g
        .edges
        .iter()
        .enumerate()
        .filter(|(i, e)| !back[*i] && e.from != e.to)
        .map(|(_, e)| e)
        .collect();
    // Ranks: longest path from the sources.
    let mut rank = vec![0usize; n];
    let mut indeg = vec![0usize; n];
    for e in &forward {
        indeg[e.to] += 1;
    }
    let mut queue: Vec<usize> = (0..n).filter(|&v| indeg[v] == 0).collect();
    let mut qi = 0;
    while qi < queue.len() {
        let v = queue[qi];
        qi += 1;
        for e in forward.iter().filter(|e| e.from == v) {
            rank[e.to] = rank[e.to].max(rank[v] + 1);
            indeg[e.to] -= 1;
            if indeg[e.to] == 0 {
                queue.push(e.to);
            }
        }
    }
    let ranks = rank.iter().max().copied().unwrap_or(0) + 1;
    // Items: nodes plus a dummy per rank a long edge crosses.
    let label_w = if lr { 18 } else { 22 };
    let mut items: Vec<Item> = (0..n)
        .map(|v| {
            let lines = wrap_label(&g.nodes[v].label, label_w);
            let w = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) + 4;
            Item {
                node: Some(v),
                w,
                h: lines.len() + 2,
                lines,
                x: 0,
                y: 0,
            }
        })
        .collect();
    let mut item_rank: Vec<usize> = rank.clone();
    // Segments between items, with the original edge (index into `forward`).
    let mut segs: Vec<(usize, usize, usize, bool)> = Vec::new(); // (from, to, edge, last)
    for (ei, e) in forward.iter().enumerate() {
        let mut prev = e.from;
        for r in rank[e.from] + 1..rank[e.to] {
            items.push(Item {
                node: None,
                w: 1,
                h: 1,
                lines: Vec::new(),
                x: 0,
                y: 0,
            });
            item_rank.push(r);
            let d = items.len() - 1;
            segs.push((prev, d, ei, false));
            prev = d;
        }
        segs.push((prev, e.to, ei, true));
    }
    // Order within ranks: first appearance, then barycenter sweeps.
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); ranks];
    for (i, &r) in item_rank.iter().enumerate() {
        layers[r].push(i);
    }
    let mut pos = vec![0f64; items.len()];
    let set_pos = |layers: &Vec<Vec<usize>>, pos: &mut Vec<f64>| {
        for l in layers {
            for (k, &i) in l.iter().enumerate() {
                pos[i] = k as f64;
            }
        }
    };
    set_pos(&layers, &mut pos);
    for sweep in 0..6 {
        let down = sweep % 2 == 0;
        let order: Vec<usize> = if down {
            (1..ranks).collect()
        } else {
            (0..ranks.saturating_sub(1)).rev().collect()
        };
        for r in order {
            let mut keyed: Vec<(f64, usize)> = layers[r]
                .iter()
                .map(|&i| {
                    let nb: Vec<f64> = segs
                        .iter()
                        .filter_map(|&(a, b, _, _)| {
                            if down && b == i {
                                Some(pos[a])
                            } else if !down && a == i {
                                Some(pos[b])
                            } else {
                                None
                            }
                        })
                        .collect();
                    let key = if nb.is_empty() {
                        pos[i]
                    } else {
                        nb.iter().sum::<f64>() / nb.len() as f64
                    };
                    (key, i)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
            layers[r] = keyed.into_iter().map(|(_, i)| i).collect();
            set_pos(&layers, &mut pos);
        }
    }
    // Coordinates.
    let gap_between = 3;
    let lanes_into: Vec<usize> = (0..ranks)
        .map(|r| {
            let mut srcs: Vec<usize> = segs
                .iter()
                .filter(|&&(a, _, _, _)| item_rank[a] == r)
                .map(|&(a, _, _, _)| a)
                .collect();
            srcs.sort();
            srcs.dedup();
            srcs.len()
        })
        .collect();
    let has_label = |r: usize| {
        segs.iter()
            .any(|&(a, _, ei, last)| item_rank[a] == r && last && !forward[ei].label.is_empty())
    };
    let longest_label = forward
        .iter()
        .map(|e| e.label.chars().count())
        .max()
        .unwrap_or(0);
    let mut lane_of: HashMap<usize, usize> = HashMap::new();
    for (r, layer) in layers.iter().enumerate() {
        let mut k = 0;
        for &i in layer {
            if segs.iter().any(|&(a, _, _, _)| a == i) {
                lane_of.insert(i, k);
                k += 1;
            }
        }
        let _ = r;
    }
    if !lr {
        let row_h: Vec<usize> = layers
            .iter()
            .map(|l| l.iter().map(|&i| items[i].h).max().unwrap_or(1))
            .collect();
        let gaps: Vec<usize> = (0..ranks)
            .map(|r| (lanes_into[r].min(4) + 2 + usize::from(has_label(r))).max(3))
            .collect();
        let widths: Vec<usize> = layers
            .iter()
            .map(|l| {
                l.iter().map(|&i| items[i].w).sum::<usize>()
                    + gap_between * l.len().saturating_sub(1)
            })
            .collect();
        let total_w = widths.iter().copied().max().unwrap_or(1);
        let mut y = 0;
        for (r, layer) in layers.iter().enumerate() {
            let mut x = (total_w - widths[r]) / 2;
            for &i in layer {
                items[i].x = x;
                items[i].y = y + (row_h[r] - items[i].h) / 2;
                x += items[i].w + gap_between;
            }
            y += row_h[r] + gaps[r];
        }
        let mut c = Canvas::new(total_w + longest_label + 4, y + 2);
        for it in &items {
            if let Some(v) = it.node {
                c.boxed(it.x, it.y, it.w, it.h, g.nodes[v].shape, &it.lines);
            }
        }
        let row_y: Vec<usize> = layers
            .iter()
            .map(|l| l.iter().map(|&i| items[i].y).min().unwrap_or(0))
            .collect();
        for &(a, b, ei, last) in &segs {
            let e = forward[ei];
            let (ia, ib) = (&items[a], &items[b]);
            let ax = ia.x + ia.w / 2;
            let bx = ib.x + ib.w / 2;
            let ra = item_rank[a];
            let start = if ia.node.is_some() { ia.y + ia.h } else { ia.y };
            let top_b = ib.y;
            let lane = lane_of.get(&a).copied().unwrap_or(0).min(3);
            let mid = (row_y[ra] + row_h[ra] + 1 + lane).min(top_b.saturating_sub(2).max(start));
            let end = if ib.node.is_some() {
                top_b.saturating_sub(1)
            } else {
                top_b
            };
            c.vline(ax, start, mid, e.stroke);
            c.hline(mid, ax, bx, e.stroke);
            c.vline(bx, mid, end, e.stroke);
            if ib.node.is_none() {
                // Through the dummy's rank.
                c.vline(
                    bx,
                    ib.y,
                    row_y[item_rank[b]] + row_h[item_rank[b]],
                    e.stroke,
                );
            }
            if last {
                if e.head != ' ' {
                    c.put(bx, end, e.head);
                }
                if e.tail && ia.node.is_some() {
                    c.put(ax, start, '▲');
                }
                if !e.label.is_empty() {
                    c.label(bx + 2, end.saturating_sub(1), &e.label);
                }
            }
        }
        let mut out = c.lines();
        out.extend(loops(&g, &back));
        return Ok(out);
    }
    // Left to right.
    let col_w: Vec<usize> = layers
        .iter()
        .map(|l| l.iter().map(|&i| items[i].w).max().unwrap_or(1))
        .collect();
    let gaps: Vec<usize> = (0..ranks)
        .map(|r| {
            let lab = if has_label(r) {
                longest_label.min(24) + 3
            } else {
                0
            };
            (lanes_into[r].min(4) + 4).max(lab + 2).max(6)
        })
        .collect();
    let heights: Vec<usize> = layers
        .iter()
        .map(|l| l.iter().map(|&i| items[i].h).sum::<usize>() + l.len().saturating_sub(1))
        .collect();
    let total_h = heights.iter().copied().max().unwrap_or(1);
    let mut x = 0;
    for (r, layer) in layers.iter().enumerate() {
        let mut y = (total_h - heights[r]) / 2;
        for &i in layer {
            items[i].x = x + (col_w[r] - items[i].w) / 2;
            items[i].y = y;
            y += items[i].h + 1;
        }
        x += col_w[r] + gaps[r];
    }
    let mut c = Canvas::new(x + 2, total_h + 2);
    for it in &items {
        if let Some(v) = it.node {
            c.boxed(it.x, it.y, it.w, it.h, g.nodes[v].shape, &it.lines);
        }
    }
    let col_x: Vec<usize> = layers
        .iter()
        .map(|l| l.iter().map(|&i| items[i].x).min().unwrap_or(0))
        .collect();
    for &(a, b, ei, last) in &segs {
        let e = forward[ei];
        let (ia, ib) = (&items[a], &items[b]);
        let ay = ia.y + ia.h / 2;
        let by = ib.y + ib.h / 2;
        let ra = item_rank[a];
        let start = if ia.node.is_some() { ia.x + ia.w } else { ia.x };
        let lane = lane_of.get(&a).copied().unwrap_or(0).min(3);
        let mid = col_x[ra] + col_w[ra] + 1 + lane;
        let end = if ib.node.is_some() {
            ib.x.saturating_sub(1)
        } else {
            ib.x
        };
        c.hline(ay, start, mid, e.stroke);
        c.vline(mid, ay, by, e.stroke);
        c.hline(by, mid, end, e.stroke);
        if ib.node.is_none() {
            c.hline(
                by,
                ib.x,
                col_x[item_rank[b]] + col_w[item_rank[b]],
                e.stroke,
            );
        }
        if last {
            if e.head != ' ' {
                c.put(end, by, if e.head == '▼' { '▶' } else { e.head });
            }
            if e.tail && ia.node.is_some() {
                c.put(start, ay, '◀');
            }
            if !e.label.is_empty() {
                let room = end.saturating_sub(mid + 2);
                let l: String = e.label.chars().take(room).collect();
                c.label(mid + 2, by.saturating_sub(1), &l);
            }
        }
    }
    let mut out = c.lines();
    out.extend(loops(&g, &back));
    Ok(out)
}

/// Edges that close a cycle (and self-loops), listed under the drawing.
fn loops(g: &Graph, back: &[bool]) -> Vec<String> {
    let mut out = Vec::new();
    for (i, e) in g.edges.iter().enumerate() {
        if back[i] || e.from == e.to {
            if out.is_empty() {
                out.push(String::new());
            }
            let label = if e.label.is_empty() {
                String::new()
            } else {
                format!("  ({})", e.label)
            };
            out.push(format!(
                "↺ {} → {}{label}",
                g.nodes[e.from].label, g.nodes[e.to].label
            ));
        }
    }
    out
}

// ── Sequence diagram ───────────────────────────────────────────────────

enum Step {
    Msg {
        from: usize,
        to: usize,
        text: String,
        dashed: bool,
        head: char,
    },
    Note {
        over: Vec<usize>,
        text: String,
    },
    Block(String),
}

fn sequence(src: &str) -> Result<Vec<String>, String> {
    let mut names: Vec<(String, String)> = Vec::new(); // (id, label)
    let mut steps: Vec<Step> = Vec::new();
    let mut number = false;
    let idx = |id: &str, names: &mut Vec<(String, String)>| -> usize {
        if let Some(i) = names.iter().position(|(n, _)| n == id) {
            return i;
        }
        names.push((id.to_string(), id.to_string()));
        names.len() - 1
    };
    let arrows = ["-->>", "->>", "-->", "->", "--x", "-x", "--)", "-)"];
    for raw in src
        .lines()
        .skip_while(|l| !l.trim().starts_with("sequenceDiagram"))
        .skip(1)
    {
        let line = raw.split("%%").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let kw = line.split_whitespace().next().unwrap_or("");
        match kw {
            "participant" | "actor" => {
                let rest = line[kw.len()..].trim();
                let (id, label) = match rest.split_once(" as ") {
                    Some((i, l)) => (i.trim(), l.trim()),
                    None => (rest, rest),
                };
                let i = idx(id, &mut names);
                names[i].1 = label.to_string();
            }
            "autonumber" => number = true,
            "Note" | "note" => {
                let rest = line[kw.len()..].trim();
                let (place, text) = rest.split_once(':').unwrap_or((rest, ""));
                let who = place
                    .trim_start_matches("over")
                    .trim_start_matches("left of")
                    .trim_start_matches("right of")
                    .trim();
                let over = who.split(',').map(|w| idx(w.trim(), &mut names)).collect();
                steps.push(Step::Note {
                    over,
                    text: text.trim().to_string(),
                });
            }
            "loop" | "alt" | "else" | "opt" | "par" | "and" | "critical" | "break" | "rect"
            | "end" => {
                steps.push(Step::Block(line.to_string()));
            }
            "activate" | "deactivate" | "title" | "box" => {}
            _ => {
                let Some((pos, arrow)) = arrows
                    .iter()
                    .filter_map(|a| line.find(a).map(|p| (p, *a)))
                    .min_by_key(|(p, a)| (*p, std::cmp::Reverse(a.len())))
                else {
                    continue;
                };
                let from = line[..pos].trim();
                let rest = &line[pos + arrow.len()..];
                let (to, text) = rest.split_once(':').unwrap_or((rest, ""));
                let to = to.trim().trim_start_matches(['+', '-']).trim();
                if from.is_empty() || to.is_empty() {
                    continue;
                }
                let (f, t) = (idx(from, &mut names), idx(to, &mut names));
                steps.push(Step::Msg {
                    from: f,
                    to: t,
                    text: text.trim().to_string(),
                    dashed: arrow.starts_with("--"),
                    head: if arrow.ends_with('x') {
                        'x'
                    } else if arrow.ends_with(')') {
                        ')'
                    } else if arrow.ends_with(">>") {
                        '>'
                    } else {
                        '-'
                    },
                });
            }
        }
    }
    if names.is_empty() {
        return Err("no participants in the diagram".into());
    }
    let k = names.len();
    let box_w: Vec<usize> = names
        .iter()
        .map(|(_, l)| l.chars().count().min(24) + 4)
        .collect();
    // Gap between neighbours: room for their boxes and the messages between.
    let mut gap = vec![0usize; k.saturating_sub(1)];
    for i in 0..k.saturating_sub(1) {
        gap[i] = box_w[i] / 2 + box_w[i + 1] / 2 + 3;
    }
    let mut n = 0;
    for s in &steps {
        if let Step::Msg { from, to, text, .. } = s
            && from != to
        {
            n += 1;
            let label = if number {
                format!("{n}. {text}")
            } else {
                text.clone()
            };
            let need = label.chars().count() + 6;
            let (a, b) = if from < to {
                (*from, *to)
            } else {
                (*to, *from)
            };
            let have: usize = gap[a..b].iter().sum();
            if have < need {
                gap[b - 1] += need - have;
            }
        }
    }
    let mut cx = vec![box_w[0] / 2; k];
    for i in 1..k {
        cx[i] = cx[i - 1] + gap[i - 1];
    }
    let self_room = steps
        .iter()
        .filter_map(|s| match s {
            Step::Msg { from, to, text, .. } if from == to => Some(text.chars().count() + 6),
            Step::Note { text, .. } => Some(text.chars().count() + 6),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let width = cx[k - 1] + box_w[k - 1] / 2 + 2 + self_room;
    let height = 3 + steps.len() * 2 + 2;
    let mut c = Canvas::new(width.max(10), height);
    for (i, (_, label)) in names.iter().enumerate() {
        let l: String = label.chars().take(24).collect();
        c.boxed(cx[i] - box_w[i] / 2, 0, box_w[i], 3, Shape::Square, &[l]);
    }
    let mut y = 3;
    let mut n = 0;
    for s in &steps {
        match s {
            Step::Msg {
                from,
                to,
                text,
                dashed,
                head,
            } => {
                let label = if number && from != to {
                    n += 1;
                    format!("{n}. {text}")
                } else {
                    text.clone()
                };
                let stroke = if *dashed {
                    Stroke::Dotted
                } else {
                    Stroke::Solid
                };
                if from == to {
                    c.label(cx[*from] + 2, y + 1, &format!("↺ {label}"));
                } else {
                    let (a, b) = (cx[*from], cx[*to]);
                    let (l, r) = if a < b {
                        (a + 1, b - 1)
                    } else {
                        (b + 1, a - 1)
                    };
                    c.hline(y + 1, l, r, stroke);
                    let tip = match (head, a < b) {
                        ('x', _) => '✕',
                        (')', true) => '▷',
                        (')', false) => '◁',
                        (_, true) => '▶',
                        (_, false) => '◀',
                    };
                    c.put(if a < b { r } else { l }, y + 1, tip);
                    let len = label.chars().count();
                    let start = l + (r - l).saturating_sub(len) / 2;
                    c.text(start, y, &label);
                }
            }
            Step::Note { over, text } => {
                let at = over.iter().map(|&i| cx[i]).min().unwrap_or(0);
                c.label(at + 2, y + 1, &format!("[{text}]"));
            }
            Step::Block(t) => {
                c.label(0, y + 1, &format!("┄ {t}"));
            }
        }
        y += 2;
    }
    // Lifelines last: only through empty cells.
    for &x in &cx {
        for yy in 3..y + 1 {
            c.mark(x, yy, UP | DOWN, Stroke::Solid);
        }
    }
    Ok(c.lines())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_mermaid_blocks() {
        let t = "Here:\n```mermaid\ngraph TD\nA-->B\n```\nand\n```rust\nfn x(){}\n```\n```Mermaid\nsequenceDiagram\n```";
        let d = extract(t);
        assert_eq!(d.len(), 2);
        assert!(d[0].contains("A-->B"));
    }

    #[test]
    fn parses_mermaid_edges() {
        let (g, lr) = parse_flowchart(
            "flowchart LR\n  A[Start] -->|go| B{Check?}\n  B -- yes --> C((Done))\n  B -.-> D & E\n  D ==> E;E---A",
        );
        assert!(lr);
        let label = |i: usize| g.nodes[i].label.as_str();
        assert_eq!(label(g.ids["A"]), "Start");
        assert_eq!(g.nodes[g.ids["B"]].shape, Shape::Decision);
        assert_eq!(g.nodes[g.ids["C"]].shape, Shape::Round);
        assert_eq!(g.edges.len(), 6);
        assert_eq!(g.edges[0].label, "go");
        assert_eq!(g.edges[1].label, "yes");
        assert_eq!(g.edges[2].stroke, Stroke::Dotted);
        assert_eq!(g.edges[4].stroke, Stroke::Thick);
        assert_eq!(g.edges[5].head, ' ');
    }

    #[test]
    fn draws_a_top_down_flowchart() {
        let out = render("graph TD\nA[Request] --> B[Auth]\nB -->|ok| C[Handler]\nB -->|no| D[401]\nC --> E[(DB)]\nA --> E").unwrap();
        let s = out.join("\n");
        for w in ["Request", "Auth", "Handler", "401", "DB", "ok", "no"] {
            assert!(s.contains(w), "{w} missing:\n{s}");
        }
        assert!(s.contains('▼'), "{s}");
        // Request is above Auth, which is above Handler.
        let row = |w: &str| out.iter().position(|l| l.contains(w)).unwrap();
        assert!(
            row("Request") < row("Auth") && row("Auth") < row("Handler"),
            "{s}"
        );
    }

    #[test]
    fn draws_left_to_right_and_lists_cycles() {
        let out = render("graph LR\nA[One] --> B[Two] --> C[Three]\nC --> A").unwrap();
        let s = out.join("\n");
        let one = out.iter().find(|l| l.contains("One")).unwrap();
        assert!(
            one.find("One") < one.find("Three").or(Some(usize::MAX)),
            "{s}"
        );
        assert!(s.contains('▶'), "{s}");
        assert!(s.contains("↺ Three → One"), "{s}");
    }

    #[test]
    fn draws_a_sequence_diagram() {
        let out = render(
            "sequenceDiagram\nparticipant U as User\nparticipant S as Server\nU->>S: login\nS-->>U: token\nNote over S: checks DB\nloop every minute\nU->>U: refresh\nend",
        )
        .unwrap();
        let s = out.join("\n");
        for w in [
            "User",
            "Server",
            "login",
            "token",
            "[checks DB]",
            "loop every minute",
            "↺ refresh",
        ] {
            assert!(s.contains(w), "{w} missing:\n{s}");
        }
        assert!(s.contains('▶') && s.contains('◀'), "{s}");
    }

    #[test]
    fn unsupported_kinds_say_so() {
        assert!(
            render("pie title Pets\n\"Dogs\": 3")
                .unwrap_err()
                .contains("pie")
        );
    }
}
#[cfg(test)]
mod show {
    #[test]
    #[ignore]
    fn print() {
        for src in [
            "graph TD\nA[Request] --> B[Auth]\nB -->|ok| C[Handler]\nB -->|no| D[401]\nC --> E[(DB)]\nA --> E",
            "flowchart LR\n  A[Start] -->|go| B{Check?}\n  B -- yes --> C((Done))\n  B -.-> D[Retry] & E[Log]\n  D ==> E",
            "sequenceDiagram\nautonumber\nparticipant U as User\nparticipant S as Server\nparticipant D as Database\nU->>S: login(name, pw)\nS->>D: find user\nD-->>S: row\nS-->>U: token\nNote over S: checks DB\nloop every minute\nU->>U: refresh\nend",
        ] {
            println!("{}\n", super::render(src).unwrap().join("\n"));
        }
    }
}
