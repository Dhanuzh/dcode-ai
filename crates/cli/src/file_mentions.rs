//! Inline `@path` file mentions: discovery and completion (TUI-side).
//!
//! Parsing/expansion moved to `dcode_ai_common::mentions` so the runtime's
//! service loop can expand prompts arriving over IPC (web chat, `serve`) too;
//! re-exported here for the existing CLI call sites.

use dcode_ai_common::mentions::is_at_mention_boundary;
pub use dcode_ai_common::mentions::{expand_at_file_mentions_default, parse_at_mentions};

use std::path::{Path, PathBuf};

const DISCOVER_MAX_FILES: usize = 4000;
const DISCOVER_MAX_DEPTH: usize = 12;

fn skip_dir_name(name: &str) -> bool {
    matches!(
        name,
        ".git" | "target" | "node_modules" | ".dcode-ai" | "dist" | "build"
    )
}

/// Most paths `git ls-files` may contribute (huge monorepos stay responsive).
const GIT_MAX_FILES: usize = 50_000;

/// Files for `@` completion, plus their folders (with a trailing `/`). In a
/// git repo this is `git ls-files` — tracked and new files, `.gitignore`
/// respected; elsewhere a bounded walk.
pub fn discover_workspace_files(workspace: &Path) -> Vec<String> {
    let files = git_files(workspace).unwrap_or_else(|| walk_files(workspace));
    with_folders(files)
}

/// Tracked plus untracked-but-not-ignored files, relative to `workspace`.
fn git_files(workspace: &Path) -> Option<Vec<String>> {
    let out = std::process::Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .current_dir(workspace)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let mut files: Vec<String> = out
        .stdout
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .take(GIT_MAX_FILES)
        .map(|p| String::from_utf8_lossy(p).to_string())
        .filter(|p| !p.starts_with(".dcode-ai/"))
        .collect();
    files.sort();
    files.dedup();
    Some(files)
}

/// Add each file's parent folders as `dir/` entries.
fn with_folders(files: Vec<String>) -> Vec<String> {
    let mut dirs: std::collections::BTreeSet<String> = Default::default();
    for f in &files {
        let mut end = 0;
        while let Some(i) = f[end..].find('/') {
            end += i + 1;
            dirs.insert(f[..end].to_string());
        }
    }
    let mut out = files;
    out.extend(dirs);
    out
}

/// Fuzzy match of `query` against `path` (case-insensitive subsequence).
/// Higher is better; `None` when not every query char appears in order.
/// Favours matches in the file name, at word starts and in a row, and
/// shorter paths — like Claude Code's `@` picker. Also returns the matched
/// char positions for highlighting.
pub fn fuzzy_score(query: &str, path: &str) -> Option<(i64, Vec<usize>)> {
    let q: Vec<char> = query.to_lowercase().chars().collect();
    if q.is_empty() {
        return Some((0, Vec::new()));
    }
    let p: Vec<char> = path.chars().collect();
    let lower: Vec<char> = p.iter().map(|c| c.to_ascii_lowercase()).collect();
    let trimmed = path.trim_end_matches('/');
    let name_start = trimmed
        .rfind('/')
        .map_or(0, |i| trimmed[..=i].chars().count());
    let attempt = |from: usize| -> Option<Vec<usize>> {
        let mut pos = Vec::with_capacity(q.len());
        let mut i = from;
        for qc in &q {
            while i < lower.len() && lower[i] != *qc {
                i += 1;
            }
            if i == lower.len() {
                return None;
            }
            pos.push(i);
            i += 1;
        }
        Some(pos)
    };
    // Prefer a match that lies entirely in the file name.
    let positions = attempt(name_start).or_else(|| attempt(0))?;
    let mut score: i64 = 0;
    let mut prev: Option<usize> = None;
    for &i in &positions {
        score += 10;
        if i >= name_start {
            score += 8;
        }
        let boundary = i == 0 || matches!(p[i - 1], '/' | '_' | '-' | '.' | ' ');
        if boundary || (p[i].is_uppercase() && p[i - 1].is_lowercase()) {
            score += 12;
        }
        if prev.is_some_and(|pv| pv + 1 == i) {
            score += 15;
        }
        prev = Some(i);
    }
    let name: String = lower[name_start..].iter().collect();
    let name = name.trim_end_matches('/');
    let ql: String = q.iter().collect();
    if name.starts_with(&ql) {
        score += 40;
    }
    if name == ql || name.split('.').next() == Some(ql.as_str()) {
        score += 60;
    }
    score -= p.len() as i64 / 4;
    score -= path.matches('/').count() as i64 * 2;
    Some((score, positions))
}

/// The best matches for `query`, best first. With no query: shallow paths
/// first, files before folders.
pub fn rank_paths(paths: &[String], query: &str, limit: usize) -> Vec<String> {
    let q = query.trim();
    if q.is_empty() {
        let mut v: Vec<&String> = paths.iter().collect();
        v.sort_by_key(|p| {
            let depth = p.trim_end_matches('/').matches('/').count();
            (depth, p.ends_with('/'), p.len())
        });
        return v.into_iter().take(limit).cloned().collect();
    }
    let mut scored: Vec<(i64, &String)> = paths
        .iter()
        .filter_map(|p| fuzzy_score(q, p).map(|(s, _)| (s, p)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.len().cmp(&b.1.len())));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, p)| p.clone())
        .collect()
}

/// Bounded walk for folders that are not git repos.
fn walk_files(workspace: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = vec![(workspace.to_path_buf(), 0)];

    while let Some((dir, depth)) = stack.pop() {
        if out.len() >= DISCOVER_MAX_FILES {
            break;
        }
        if depth > DISCOVER_MAX_DEPTH {
            continue;
        }
        let read_dir = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for ent in read_dir.flatten() {
            if out.len() >= DISCOVER_MAX_FILES {
                break;
            }
            let name = ent.file_name().to_string_lossy().to_string();
            if skip_dir_name(&name) {
                continue;
            }
            let path = ent.path();
            let Ok(ft) = ent.file_type() else {
                continue;
            };
            if ft.is_dir() {
                stack.push((path, depth + 1));
            } else if ft.is_file()
                && let Ok(rel) = path.strip_prefix(workspace)
            {
                let s = rel.to_string_lossy().replace('\\', "/");
                if !s.is_empty() && !s.starts_with("../") {
                    out.push(s);
                }
            }
        }
    }

    out.sort();
    out.dedup();
    out
}

/// Paths whose relative path (unix-style) starts with `prefix` (case-sensitive on Unix).
pub fn filter_paths_prefix(paths: &[String], prefix: &str) -> Vec<String> {
    // Typing a folder path (`src/tui/`) lists what is inside it.
    if let Some((dir, rest)) = prefix.rsplit_once('/') {
        let dir = format!("{dir}/");
        // Ranked by the part after the folder, so `src/t` prefers `tui/…`.
        let inside: Vec<String> = paths
            .iter()
            .filter(|p| p.starts_with(&dir) && p.len() > dir.len())
            // Direct children only while nothing is typed after the `/`.
            .filter(|p| !rest.is_empty() || !p[dir.len()..].trim_end_matches('/').contains('/'))
            .map(|p| p[dir.len()..].to_string())
            .collect();
        let ranked = rank_paths(&inside, rest, AT_MATCH_LIMIT);
        if !ranked.is_empty() {
            return ranked.into_iter().map(|r| format!("{dir}{r}")).collect();
        }
    }
    cached_rank(paths, prefix)
}

/// Rows the `@` list keeps (eight show at once).
pub const AT_MATCH_LIMIT: usize = 50;

/// `rank_paths` for the `@` list, remembered for the last query: the list is
/// ranked again on every frame and key press otherwise.
fn cached_rank(paths: &[String], query: &str) -> Vec<String> {
    type Memo = Option<(usize, usize, String, Vec<String>)>;
    static MEMO: std::sync::Mutex<Memo> = std::sync::Mutex::new(None);
    let key = (paths.as_ptr() as usize, paths.len());
    if let Ok(m) = MEMO.lock()
        && let Some((p, n, q, v)) = m.as_ref()
        && (*p, *n) == key
        && q == query
    {
        return v.clone();
    }
    let ranked = rank_paths(paths, query, AT_MATCH_LIMIT);
    if let Ok(mut m) = MEMO.lock() {
        *m = Some((key.0, key.1, query.to_string(), ranked.clone()));
    }
    ranked
}

/// Quick symbol search: grep for `fn|struct|enum|trait|type|const|impl` lines
/// matching `query` in the workspace and return `symbol_name  (file:line)` entries.
/// Uses ripgrep when available; falls back to an empty list on failure.
pub fn search_workspace_symbols(workspace: &Path, query: &str) -> Vec<String> {
    if query.len() < 2 || query.contains('/') {
        return Vec::new();
    }
    // Build a ripgrep pattern that matches Rust/TS/JS symbol declarations.
    let pattern = format!(
        r"(fn|struct|enum|trait|type|const|class|interface|def|func)\s+{q}",
        q = regex_escape_simple(query)
    );
    let output = std::process::Command::new("rg")
        .args([
            "--no-heading",
            "--line-number",
            "--max-count=40",
            "--glob=!target",
            "--glob=!node_modules",
            "--glob=!.dcode-ai",
            &pattern,
        ])
        .current_dir(workspace)
        .output();
    let Ok(out) = output else {
        return Vec::new();
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut results: Vec<String> = Vec::new();
    for line in stdout.lines().take(40) {
        // Format: path:line:content
        let parts: Vec<&str> = line.splitn(3, ':').collect();
        if parts.len() < 3 {
            continue;
        }
        let file = parts[0];
        let lineno = parts[1];
        let content = parts[2].trim();
        // Extract just the symbol name from content using simple heuristic.
        let sym = extract_symbol_name(content).unwrap_or(query);
        let entry = format!("@sym:{sym}  ({file}:{lineno})");
        results.push(entry);
    }
    results
}

fn regex_escape_simple(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if "()[]{}.*+?^$|\\".contains(c) {
                vec!['\\', c]
            } else {
                vec![c]
            }
        })
        .collect()
}

fn extract_symbol_name(line: &str) -> Option<&str> {
    // Skip keyword + whitespace, take the identifier.
    let after_kw = line
        .find(|c: char| c.is_alphabetic())
        .and_then(|i| line[i..].find(char::is_whitespace).map(|j| &line[i + j..]))?;
    let trimmed = after_kw.trim_start();
    let end = trimmed
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(trimmed.len());
    if end == 0 {
        None
    } else {
        Some(&trimmed[..end])
    }
}

/// Active `@`-token immediately before `cursor_byte` (UTF-8 byte index in `line`).
pub fn at_token_before_cursor(line: &str, cursor_byte: usize) -> Option<(usize, String)> {
    let before = line.get(..cursor_byte.min(line.len()))?;
    let at_rel = before.rfind('@')?;
    let prev = at_rel
        .checked_sub(1)
        .and_then(|i| before.get(i..))
        .and_then(|s| s.chars().next());
    if !is_at_mention_boundary(prev) {
        return None;
    }
    let after = &before[at_rel + 1..];
    if after.chars().any(|c| c.is_whitespace()) {
        return None;
    }
    let token = after.replace('\\', "/");
    Some((at_rel, token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_two_mentions() {
        let s = "See @crates/foo.rs and @README.md ok";
        let m = parse_at_mentions(s);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0], (4, 18, "crates/foo.rs".into()));
        assert_eq!(m[1], (23, 33, "README.md".into()));
    }

    #[test]
    fn fuzzy_finds_files_by_name_anywhere_in_the_tree() {
        let paths: Vec<String> = [
            "Cargo.toml",
            "crates/autoresearch/src/git_integration.rs",
            "crates/cli/src/tui/git.rs",
            "crates/cli/src/tui/",
            "docs/plans/gitignore-notes.md",
            "README.md",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let r = rank_paths(&paths, "git", 10);
        assert_eq!(r[0], "crates/cli/src/tui/git.rs", "{r:?}");
        assert!(r.contains(&"crates/autoresearch/src/git_integration.rs".to_string()));
        let r = rank_paths(&paths, "gitint", 10);
        assert_eq!(r[0], "crates/autoresearch/src/git_integration.rs");
        assert!(rank_paths(&paths, "zzz", 10).is_empty());
        // Empty query: shallow first.
        assert_eq!(rank_paths(&paths, "", 2), vec!["README.md", "Cargo.toml"]);
        // A folder path lists its children.
        let r = filter_paths_prefix(&paths, "crates/cli/src/tui/");
        assert_eq!(r, vec!["crates/cli/src/tui/git.rs"]);
        let (_, pos) = fuzzy_score("gr", "crates/cli/src/tui/git.rs").unwrap();
        assert_eq!(pos, vec![19, 23]);
    }

    #[test]
    fn folders_are_listed_with_a_trailing_slash() {
        let out = with_folders(vec!["a/b/c.rs".into(), "a/d.rs".into()]);
        assert!(out.contains(&"a/".to_string()) && out.contains(&"a/b/".to_string()));
    }

    #[test]
    fn at_token_detects_active_path() {
        let line = "hi @src/main.rs";
        let cur = line.len();
        let (i, tok) = at_token_before_cursor(line, cur).unwrap();
        assert_eq!(i, 3);
        assert_eq!(tok, "src/main.rs");
    }
}
