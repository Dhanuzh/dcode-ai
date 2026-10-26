//! Instructions in subfolders (`AGENTS.md`, `CLAUDE.md`, `CLAUDE.local.md`),
//! loaded the first time the agent touches a path under them — Claude Code's
//! nested memory. The workspace root's own files are in the system prompt.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

const NAMES: [&str; 3] = ["AGENTS.md", "CLAUDE.md", "CLAUDE.local.md"];
/// Longest instruction file passed on.
const MAX_CHARS: usize = 8_000;

/// Instruction files between `workspace_root` (exclusive) and `path` not yet
/// in `loaded`, outermost first, as one block to append to a tool result.
pub fn for_path(
    workspace_root: &Path,
    path: &str,
    loaded: &mut HashSet<PathBuf>,
) -> Option<String> {
    let rel = Path::new(path);
    let rel = rel.strip_prefix(workspace_root).unwrap_or(rel);
    if rel.is_absolute() || rel.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    let full = workspace_root.join(rel);
    let dir_rel = if full.is_dir() { rel } else { rel.parent()? };
    let mut dirs = Vec::new();
    let mut acc = PathBuf::new();
    for c in dir_rel.components() {
        let Component::Normal(part) = c else {
            continue;
        };
        acc.push(part);
        dirs.push(acc.clone());
    }
    let mut out = String::new();
    for dir in dirs {
        for name in NAMES {
            let rel_file = dir.join(name);
            if !loaded.insert(rel_file.clone()) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(workspace_root.join(&rel_file)) else {
                continue;
            };
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            let text: String = text.chars().take(MAX_CHARS).collect();
            // Same spelling on every OS.
            let slash = |p: &Path| p.display().to_string().replace('\\', "/");
            out.push_str(&format!(
                "\n\n<instructions source=\"{}\">\nThese instructions apply to work under {}/ \
                 (they add to the project's own instructions):\n{text}\n</instructions>",
                slash(&rel_file),
                slash(&dir)
            ));
        }
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subfolder_instructions_load_once_outermost_first() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("web/src")).unwrap();
        std::fs::write(root.join("AGENTS.md"), "root rules").unwrap();
        std::fs::write(root.join("web/CLAUDE.md"), "web rules").unwrap();
        std::fs::write(root.join("web/src/AGENTS.md"), "src rules").unwrap();
        std::fs::write(root.join("web/src/app.ts"), "x").unwrap();
        let mut loaded = HashSet::new();
        let text = for_path(root, "web/src/app.ts", &mut loaded).expect("instructions");
        assert!(
            !text.contains("root rules"),
            "root files are in the system prompt"
        );
        let web = text.find("web rules").unwrap();
        let src = text.find("src rules").unwrap();
        assert!(web < src);
        assert!(text.contains("source=\"web/CLAUDE.md\""));
        assert_eq!(for_path(root, "web/src/app.ts", &mut loaded), None);
        let abs = root.join("web/src/app.ts");
        assert!(for_path(root, abs.to_str().unwrap(), &mut HashSet::new()).is_some());
        assert_eq!(for_path(root, "../etc/passwd", &mut loaded), None);
    }
}
