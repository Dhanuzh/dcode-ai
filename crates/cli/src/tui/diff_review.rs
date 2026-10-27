//! Working-tree change review for the dashboard's *changes* tab.
//!
//! Lists files with unstaged changes (worktree vs the git index) plus
//! untracked files, splits each into hunks, and reverts a single hunk by
//! rewriting the file from the index content with every other hunk kept.

use std::path::Path;
use std::process::Command;

use super::diff_hunk::{DiffHunk, apply_selected_hunks, parse_diff_hunks};

#[derive(Debug, Clone)]
pub(crate) struct FileChange {
    /// Path relative to the workspace root.
    pub path: String,
    pub untracked: bool,
    pub added: usize,
    pub removed: usize,
    pub hunks: Vec<DiffHunk>,
}

/// State of the dashboard *changes* tab.
#[derive(Debug, Default)]
pub(crate) struct DiffReview {
    pub files: Vec<FileChange>,
    /// Selected hunk, counted across all files (untracked files count as one).
    pub cursor: usize,
    /// `r` was pressed once on the selected hunk; a second `r` reverts it.
    pub confirm_revert: bool,
    /// Last load/revert problem, shown in the panel.
    pub error: Option<String>,
    pub loaded: bool,
}

impl DiffReview {
    /// Re-read the working tree, keeping the cursor in range.
    pub fn refresh(&mut self, workspace: &Path) {
        self.confirm_revert = false;
        match load_changes(workspace) {
            Ok(files) => {
                self.files = files;
                self.error = None;
            }
            Err(e) => {
                self.files.clear();
                self.error = Some(e);
            }
        }
        self.loaded = true;
        self.cursor = self.cursor.min(self.item_count().saturating_sub(1));
    }

    /// Number of selectable rows: one per hunk, one per untracked file.
    pub fn item_count(&self) -> usize {
        self.files.iter().map(|f| f.hunks.len().max(1)).sum()
    }

    /// `(file index, hunk index)` of the selected row.
    pub fn selected(&self) -> Option<(usize, usize)> {
        self.locate(self.cursor)
    }

    pub fn locate(&self, item: usize) -> Option<(usize, usize)> {
        let mut rest = item;
        for (fi, file) in self.files.iter().enumerate() {
            let n = file.hunks.len().max(1);
            if rest < n {
                return Some((fi, rest));
            }
            rest -= n;
        }
        None
    }

    /// Index of the file the selection is in.
    pub fn selected_file(&self) -> usize {
        self.selected().map_or(0, |(fi, _)| fi)
    }

    /// First review item (hunk) of file `fi`.
    pub fn first_item_of(&self, fi: usize) -> usize {
        self.files[..fi.min(self.files.len())]
            .iter()
            .map(|f| f.hunks.len().max(1))
            .sum()
    }

    /// Move the selection to the next / previous file.
    pub fn move_file(&mut self, delta: isize) {
        if self.files.is_empty() {
            return;
        }
        let max = self.files.len() as isize - 1;
        let fi = (self.selected_file() as isize + delta).clamp(0, max) as usize;
        self.cursor = self.first_item_of(fi);
        self.confirm_revert = false;
    }

    /// Handle `r`: the first press arms, the second reverts the hunk.
    /// Returns a status message for the user.
    pub fn press_revert(&mut self, workspace: &Path) -> Option<String> {
        let (fi, hi) = self.selected()?;
        let file = &self.files[fi];
        if file.untracked {
            self.confirm_revert = false;
            return Some(format!(
                "{} is untracked; delete it yourself if unwanted",
                file.path
            ));
        }
        if !self.confirm_revert {
            self.confirm_revert = true;
            return Some(format!(
                "Press r again to revert hunk {} of {}",
                hi + 1,
                file.path
            ));
        }
        self.confirm_revert = false;
        let path = file.path.clone();
        let msg = match revert_hunk(workspace, &path, hi) {
            Ok(()) => format!("Reverted hunk {} of {path}", hi + 1),
            Err(e) => {
                self.error = Some(e.clone());
                format!("Revert failed: {e}")
            }
        };
        self.refresh(workspace);
        Some(msg)
    }
}

fn git(workspace: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .map_err(|e| format!("git not available: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn git_lines(workspace: &Path, args: &[&str]) -> Result<Vec<String>, String> {
    Ok(String::from_utf8_lossy(&git(workspace, args)?)
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// Index (staged) content of `path`, or empty if it is not in the index.
fn index_content(workspace: &Path, path: &str) -> Result<String, String> {
    let bytes = git(workspace, &["show", &format!(":./{path}")]).unwrap_or_default();
    String::from_utf8(bytes).map_err(|_| "binary file".to_string())
}

fn worktree_content(workspace: &Path, path: &str) -> Result<String, String> {
    let bytes = std::fs::read(workspace.join(path)).unwrap_or_default();
    String::from_utf8(bytes).map_err(|_| "binary file".to_string())
}

/// Untracked files larger than this are skipped (logs, build output).
const MAX_UNTRACKED_BYTES: u64 = 512 * 1024;

fn load_changes(workspace: &Path) -> Result<Vec<FileChange>, String> {
    git(workspace, &["rev-parse", "--is-inside-work-tree"])
        .map_err(|_| "not a git repository".to_string())?;
    let mut files = Vec::new();
    for path in git_lines(workspace, &["diff", "--name-only", "--relative"])? {
        // dcode-ai's own session state is not a change to review, even when
        // a repo happens to track it.
        if path.starts_with(".dcode-ai/") {
            continue;
        }
        let (Ok(old), Ok(new)) = (
            index_content(workspace, &path),
            worktree_content(workspace, &path),
        ) else {
            continue; // binary
        };
        let hunks = parse_diff_hunks(&old, &new);
        if hunks.is_empty() {
            continue; // mode-only or whitespace-at-EOF change
        }
        files.push(file_change(path, false, hunks));
    }
    for path in git_lines(workspace, &["ls-files", "--others", "--exclude-standard"])? {
        if path.starts_with(".dcode-ai/") {
            continue;
        }
        let too_big = std::fs::metadata(workspace.join(&path))
            .map(|m| m.len() > MAX_UNTRACKED_BYTES)
            .unwrap_or(true);
        if too_big {
            continue;
        }
        let Ok(new) = worktree_content(workspace, &path) else {
            continue;
        };
        files.push(file_change(path, true, parse_diff_hunks("", &new)));
    }
    Ok(files)
}

fn file_change(path: String, untracked: bool, hunks: Vec<DiffHunk>) -> FileChange {
    let count = |sigil| {
        hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|(s, _)| *s == sigil)
            .count()
    };
    FileChange {
        added: count('+'),
        removed: count('-'),
        path,
        untracked,
        hunks,
    }
}

/// Undo hunk `index` of `path`'s unstaged changes, keeping all other hunks.
fn revert_hunk(workspace: &Path, path: &str, index: usize) -> Result<(), String> {
    let old = index_content(workspace, path)?;
    let new = worktree_content(workspace, path)?;
    let hunks = parse_diff_hunks(&old, &new);
    if index >= hunks.len() {
        return Err("the file changed; refresh and try again".into());
    }
    let keep: Vec<bool> = (0..hunks.len()).map(|i| i != index).collect();
    let content = apply_selected_hunks(&old, &new, &keep);
    std::fs::write(workspace.join(path), content).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let run = |args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .expect("git")
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "t"]);
        let body: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.path().join("a.txt"), body).unwrap();
        run(&["add", "a.txt"]);
        run(&["commit", "-qm", "init"]);
        dir
    }

    #[test]
    fn lists_hunks_and_reverts_one() {
        let dir = repo();
        let ws = dir.path();
        let edited = std::fs::read_to_string(ws.join("a.txt"))
            .unwrap()
            .replace("line 2\n", "line two\n")
            .replace("line 28\n", "line twenty-eight\n");
        std::fs::write(ws.join("a.txt"), edited).unwrap();
        std::fs::write(ws.join("new.txt"), "hello\n").unwrap();
        std::fs::create_dir_all(ws.join(".dcode-ai/sessions")).unwrap();
        std::fs::write(ws.join(".dcode-ai/sessions/s.json"), "{}\n").unwrap();

        let mut review = DiffReview::default();
        review.refresh(ws);
        assert!(review.error.is_none(), "{:?}", review.error);
        assert_eq!(review.files.len(), 2);
        assert_eq!(review.files[0].path, "a.txt");
        assert_eq!(review.files[0].hunks.len(), 2);
        assert!(review.files[1].untracked);
        assert_eq!(review.item_count(), 3);

        // First `r` only arms; the second reverts the first hunk.
        assert!(review.press_revert(ws).unwrap().contains("again"));
        assert!(review.press_revert(ws).unwrap().starts_with("Reverted"));
        let now = std::fs::read_to_string(ws.join("a.txt")).unwrap();
        assert!(now.contains("line 2\n") && now.contains("line twenty-eight\n"));
        assert_eq!(review.files[0].hunks.len(), 1);

        // Untracked files are never deleted.
        review.cursor = 1;
        assert!(review.press_revert(ws).unwrap().contains("untracked"));
        assert!(ws.join("new.txt").exists());
    }

    #[test]
    fn outside_git_reports_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut review = DiffReview::default();
        review.refresh(dir.path());
        assert_eq!(review.error.as_deref(), Some("not a git repository"));
        assert_eq!(review.item_count(), 0);
    }
}
