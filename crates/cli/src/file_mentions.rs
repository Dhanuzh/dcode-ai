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

/// Walk workspace (bounded) and collect relative file paths for `@` completion.
pub fn discover_workspace_files(workspace: &Path) -> Vec<String> {
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
    let p = prefix.trim();
    if p.is_empty() {
        return paths.iter().take(200).cloned().collect();
    }
    let mut v: Vec<String> = paths
        .iter()
        .filter(|s| s.starts_with(p))
        .take(200)
        .cloned()
        .collect();
    if v.len() < 50 {
        let pl = p.to_ascii_lowercase();
        for s in paths {
            if v.len() >= 200 {
                break;
            }
            if s.to_ascii_lowercase().starts_with(&pl) && !v.iter().any(|x| x == s) {
                v.push(s.clone());
            }
        }
    }
    v
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
    fn at_token_detects_active_path() {
        let line = "hi @src/main.rs";
        let cur = line.len();
        let (i, tok) = at_token_before_cursor(line, cur).unwrap();
        assert_eq!(i, 3);
        assert_eq!(tok, "src/main.rs");
    }
}
