//! `@path` file-mention parsing and expansion.
//!
//! Lives in `common` so every prompt entry point shares one implementation:
//! the TUI/REPL expand before `run_turn`, and the runtime's service loop
//! expands prompts arriving over IPC (web chat, `serve`, spawned sessions) —
//! which previously didn't expand at all, so the model saw a literal `@path`.
//!
//! Expansion turns each `@rel/path` into a fenced ` ```file:rel/path ` block
//! with the file contents; the transcript preview compacts it back to `@path`
//! (see `MessageContent::user_event_preview`).

use std::path::{Component, Path, PathBuf};

pub const DEFAULT_MAX_FILE_BYTES: usize = 512 * 1024;

/// True when a `@` at this position starts a mention (start of line, after
/// whitespace, or after an opening bracket/quote).
pub fn is_at_mention_boundary(prev: Option<char>) -> bool {
    match prev {
        None => true,
        Some(c) => c.is_whitespace() || matches!(c, '(' | '[' | '{' | '<' | '"' | '\'' | '`'),
    }
}

fn is_path_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ':' | '\\')
}

/// Byte range and path string for one `@mention` (path does not include `@`).
pub fn parse_at_mentions(text: &str) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut i = 0;
    while i < chars.len() {
        let (byte_idx, c) = chars[i];
        if c == '@' {
            let prev = i.checked_sub(1).map(|j| chars[j].1);
            if is_at_mention_boundary(prev) {
                let start = byte_idx;
                i += 1;
                let path_start = i;
                while i < chars.len() && is_path_char(chars[i].1) {
                    i += 1;
                }
                if i > path_start {
                    let end_byte = if i < chars.len() {
                        chars[i].0
                    } else {
                        text.len()
                    };
                    let path: String = chars[path_start..i].iter().map(|(_, ch)| *ch).collect();
                    let path = path.replace('\\', "/");
                    if !path.is_empty() && !path.starts_with('@') {
                        out.push((start, end_byte, path));
                    }
                }
                continue;
            }
        }
        i += 1;
    }
    out
}

fn normalize_join(workspace: &Path, rel: &str) -> PathBuf {
    let rel = rel.trim_start_matches("./");
    let mut base = workspace.to_path_buf();
    for comp in Path::new(rel).components() {
        match comp {
            Component::Normal(s) => base.push(s),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return PathBuf::new();
            }
        }
    }
    base
}

/// Expand each `@rel/path` into a fenced code block with file contents (or an error note).
pub fn expand_at_file_mentions(
    text: &str,
    workspace: &Path,
    max_file_bytes: usize,
) -> anyhow::Result<String> {
    let mentions = parse_at_mentions(text);
    if mentions.is_empty() {
        return Ok(text.to_string());
    }

    let mut inserts: Vec<(usize, usize, String)> = Vec::new();
    for (start, end, rel) in mentions {
        let full = normalize_join(workspace, &rel);
        if full.as_os_str().is_empty() || !full.starts_with(workspace) {
            inserts.push((
                start,
                end,
                format!("[dcode-ai: skipped unsafe path @{rel}]"),
            ));
            continue;
        }
        match std::fs::read(&full) {
            Ok(bytes) => {
                let n = bytes.len().min(max_file_bytes);
                let slice = &bytes[..n];
                let content = String::from_utf8_lossy(slice);
                let note = if bytes.len() > max_file_bytes {
                    format!(
                        "\n… truncated ({} bytes, showing first {})\n",
                        bytes.len(),
                        n
                    )
                } else {
                    String::new()
                };
                let block = format!("\n\n```file:{rel}\n{}{}\n```\n\n", content, note);
                inserts.push((start, end, block));
            }
            Err(e) => {
                inserts.push((
                    start,
                    end,
                    format!("[dcode-ai: could not read @{rel}: {e}]"),
                ));
            }
        }
    }

    inserts.sort_by_key(|(s, _, _)| *s);
    let mut offset: isize = 0;
    let mut result = text.to_string();
    for (start, end, replacement) in inserts {
        let s = ((start as isize) + offset).max(0) as usize;
        let e = ((end as isize) + offset).max(0) as usize;
        if s <= e && e <= result.len() {
            result.replace_range(s..e, &replacement);
            offset += replacement.len() as isize - (end - start) as isize;
        }
    }
    Ok(result)
}

/// Expand mentions with default size limit.
pub fn expand_at_file_mentions_default(text: &str, workspace: &Path) -> anyhow::Result<String> {
    expand_at_file_mentions(text, workspace, DEFAULT_MAX_FILE_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_boundary_and_path_chars() {
        let mentions = parse_at_mentions("see @src/main.rs and (@docs/a.md) but not user@host");
        let paths: Vec<&str> = mentions.iter().map(|(_, _, p)| p.as_str()).collect();
        assert_eq!(paths, vec!["src/main.rs", "docs/a.md"]);
    }

    #[test]
    fn expands_file_into_fenced_block() {
        let dir = std::env::temp_dir().join(format!("dcode-mentions-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("hello.txt"), "hi there").unwrap();

        let out = expand_at_file_mentions_default("read @hello.txt please", &dir).unwrap();
        assert!(out.contains("```file:hello.txt\nhi there\n```"), "{out}");
        assert!(out.starts_with("read "));
        assert!(out.trim_end().ends_with("please"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn traversal_is_refused_and_missing_files_noted() {
        let dir = std::env::temp_dir();
        let out = expand_at_file_mentions_default("@../../etc/passwd", &dir).unwrap();
        assert!(out.contains("skipped unsafe path"), "{out}");
        let out = expand_at_file_mentions_default("@definitely-not-here.xyz", &dir).unwrap();
        assert!(out.contains("could not read"), "{out}");
    }
}
