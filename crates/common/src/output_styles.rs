//! Output styles (Claude Code's `/output-style`): built-in Default,
//! Explanatory and Learning, plus custom styles from Markdown files:
//!
//! ```markdown
//! ---
//! name: Reviewer
//! description: Terse code-review voice
//! ---
//! Answer like a senior reviewer: findings first, one line each …
//! ```
//!
//! Custom files live in `output-styles/` under `.dcode-ai/` or `.claude/`,
//! in the workspace (wins) or the home directory.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputStyle {
    /// Lowercase key used in config and `/output-style <name>`.
    pub key: String,
    pub name: String,
    pub description: String,
    /// Instructions added to the system prompt (empty for Default).
    pub prompt: String,
    /// Where a custom style came from (`None` for built-ins).
    pub source: Option<PathBuf>,
}

const EXPLANATORY: &str = "Output style: Explanatory\n\
You are helping with software engineering tasks and also teaching the user about the \
codebase and the choices you make. Alongside the work, add short educational notes:\n\
- Before or after writing code, explain the reasoning behind non-obvious choices and the \
trade-offs you considered.\n\
- Point out codebase-specific patterns, conventions and pitfalls you notice.\n\
- Put each note in an insight block:\n\
  `★ Insight ─────────────────────────────────────`\n\
  2-3 key points\n\
  `─────────────────────────────────────────────────`\n\
- Keep insights specific to this code, not general programming advice, and do not let them \
slow the task down.";

const LEARNING: &str = "Output style: Learning\n\
You are helping with software engineering tasks while the user learns by doing. Work \
collaboratively:\n\
- Do the routine parts yourself, but for meaningful design decisions or small pieces of \
logic (5-10 lines) that teach something, ask the user to write them.\n\
- When you hand over, add a `TODO(human)` comment at the exact spot in the code, then stop \
and explain what to write, the context, and the trade-offs. Do not write that part \
yourself, and wait for the user before continuing that part.\n\
- Share brief insight blocks about choices and patterns, as in the Explanatory style:\n\
  `★ Insight ─────────────────────────────────────`\n\
  2-3 key points\n\
  `─────────────────────────────────────────────────`";

pub fn builtin_styles() -> Vec<OutputStyle> {
    let b = |key: &str, name: &str, description: &str, prompt: &str| OutputStyle {
        key: key.into(),
        name: name.into(),
        description: description.into(),
        prompt: prompt.into(),
        source: None,
    };
    vec![
        b(
            "default",
            "Default",
            "Efficient software engineering, concise replies",
            "",
        ),
        b(
            "explanatory",
            "Explanatory",
            "Explains its implementation choices and codebase patterns",
            EXPLANATORY,
        ),
        b(
            "learning",
            "Learning",
            "Learn by doing: asks you to write small pieces of code (TODO(human))",
            LEARNING,
        ),
    ]
}

/// Parse a style file: optional `---` frontmatter (`name`, `description`),
/// then the instructions. The name defaults to the file stem.
pub fn parse_style(path: &Path, text: &str) -> Option<OutputStyle> {
    let stem = path.file_stem()?.to_string_lossy().to_string();
    let (mut name, mut description, mut body) = (stem.clone(), String::new(), text);
    if let Some(rest) = text.strip_prefix("---")
        && let Some(end) = rest.find("\n---")
    {
        for line in rest[..end].lines() {
            if let Some((k, v)) = line.split_once(':') {
                let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
                match k.trim() {
                    "name" if !v.is_empty() => name = v,
                    "description" => description = v,
                    _ => {}
                }
            }
        }
        body = rest[end + 4..].trim_start_matches(['-', '\n', '\r']);
    }
    let prompt = body.trim().to_string();
    if prompt.is_empty() {
        return None;
    }
    Some(OutputStyle {
        key: name.to_ascii_lowercase(),
        name,
        description,
        prompt,
        source: Some(path.to_path_buf()),
    })
}

fn style_dirs(workspace_root: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![
        workspace_root.join(".dcode-ai/output-styles"),
        workspace_root.join(".claude/output-styles"),
    ];
    if let Some(home) = crate::config::home_dir() {
        dirs.push(home.join(".dcode-ai/output-styles"));
        dirs.push(home.join(".claude/output-styles"));
    }
    dirs
}

/// Built-ins, then custom styles (workspace before home; first name wins).
pub fn discover(workspace_root: &Path) -> Vec<OutputStyle> {
    let mut styles = builtin_styles();
    for dir in style_dirs(workspace_root) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "md"))
            .collect();
        files.sort();
        for path in files {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Some(style) = parse_style(&path, &text)
                && !styles.iter().any(|s| s.key == style.key)
            {
                styles.push(style);
            }
        }
    }
    styles
}

/// The style selected by `key` (case-insensitive), if it exists.
pub fn find(workspace_root: &Path, key: &str) -> Option<OutputStyle> {
    let key = key.trim().to_ascii_lowercase();
    discover(workspace_root).into_iter().find(|s| s.key == key)
}

/// System-prompt section for the configured style (`None` for Default).
pub fn prompt_section(workspace_root: &Path, key: Option<&str>) -> Option<String> {
    let style = find(workspace_root, key?)?;
    (!style.prompt.is_empty()).then(|| {
        if style.prompt.starts_with("Output style:") {
            style.prompt
        } else {
            format!("Output style: {}\n{}", style.name, style.prompt)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_and_custom_styles() {
        let dir = tempfile::tempdir().unwrap();
        let styles_dir = dir.path().join(".claude/output-styles");
        std::fs::create_dir_all(&styles_dir).unwrap();
        std::fs::write(
            styles_dir.join("review.md"),
            "---\nname: Reviewer\ndescription: terse reviews\n---\nFindings first.\n",
        )
        .unwrap();
        std::fs::write(styles_dir.join("empty.md"), "---\nname: Empty\n---\n").unwrap();

        let all = discover(dir.path());
        let keys: Vec<&str> = all.iter().map(|s| s.key.as_str()).collect();
        assert!(
            keys.starts_with(&["default", "explanatory", "learning"]),
            "{keys:?}"
        );
        let review = find(dir.path(), "REVIEWER").expect("custom style");
        assert_eq!(review.description, "terse reviews");
        assert_eq!(review.prompt, "Findings first.");
        assert!(find(dir.path(), "empty").is_none());

        assert_eq!(prompt_section(dir.path(), Some("default")), None);
        assert!(
            prompt_section(dir.path(), Some("explanatory"))
                .unwrap()
                .contains("★ Insight")
        );
        assert_eq!(
            prompt_section(dir.path(), Some("reviewer")).unwrap(),
            "Output style: Reviewer\nFindings first."
        );
    }
}
