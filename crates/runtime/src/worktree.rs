//! Isolated git worktree management for agent runs.
//! Each agent run can operate in its own worktree/branch so that parallel
//! runs don't interfere with each other or the user's working tree.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Metadata about a worktree created for an agent run.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorktreeInfo {
    pub worktree_path: PathBuf,
    pub branch_name: String,
    pub base_branch: String,
    pub session_id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Pathspec for a sub-agent's own work: everything but dcode-ai's state
/// folder (its generated `.gitignore`, sessions), which the session creates.
const OWN_WORK: [&str; 2] = [".", ":(exclude).dcode-ai"];

/// Manages git worktrees for isolated agent runs.
pub struct WorktreeManager {
    repo_root: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error("git command failed: {0}")]
    GitFailed(String),
    #[error("not a git repository: {0}")]
    NotGitRepo(String),
    #[error("IO error: {0}")]
    Io(String),
}

impl WorktreeManager {
    pub fn new(repo_root: impl AsRef<Path>) -> Self {
        Self {
            repo_root: repo_root.as_ref().to_path_buf(),
        }
    }

    /// Check if the workspace is a git repository.
    pub fn is_git_repo(&self) -> bool {
        Command::new("git")
            .args(["rev-parse", "--git-dir"])
            .current_dir(&self.repo_root)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Get the current branch name.
    pub fn current_branch(&self) -> Result<String, WorktreeError> {
        let output = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(&self.repo_root)
            .output()
            .map_err(|e| WorktreeError::Io(e.to_string()))?;

        if !output.status.success() {
            return Err(WorktreeError::GitFailed(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Create an isolated worktree for an agent run.
    /// The worktree is created at `<repo>/.dcode-ai/worktrees/<session_id>` with
    /// a new branch `dcode-ai/<session_id>` based on the current HEAD.
    pub fn create_worktree(&self, session_id: &str) -> Result<WorktreeInfo, WorktreeError> {
        if !self.is_git_repo() {
            return Err(WorktreeError::NotGitRepo(
                self.repo_root.display().to_string(),
            ));
        }

        let base_branch = self.current_branch()?;
        let branch_name = format!("dcode-ai/{session_id}");
        let worktree_path = self
            .repo_root
            .join(".dcode-ai")
            .join("worktrees")
            .join(session_id);

        if worktree_path.exists() {
            std::fs::remove_dir_all(&worktree_path)
                .map_err(|e| WorktreeError::Io(e.to_string()))?;
        }

        if let Some(parent) = worktree_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| WorktreeError::Io(e.to_string()))?;
        }

        let output = Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                &branch_name,
                &worktree_path.display().to_string(),
                "HEAD",
            ])
            .current_dir(&self.repo_root)
            .output()
            .map_err(|e| WorktreeError::Io(e.to_string()))?;

        if !output.status.success() {
            return Err(WorktreeError::GitFailed(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        Ok(WorktreeInfo {
            worktree_path,
            branch_name,
            base_branch,
            session_id: session_id.to_string(),
            created_at: chrono::Utc::now(),
        })
    }

    /// Remove a worktree and optionally its branch.
    pub fn remove_worktree(
        &self,
        session_id: &str,
        delete_branch: bool,
    ) -> Result<(), WorktreeError> {
        let worktree_path = self
            .repo_root
            .join(".dcode-ai")
            .join("worktrees")
            .join(session_id);

        if worktree_path.exists() {
            let output = Command::new("git")
                .args([
                    "worktree",
                    "remove",
                    "--force",
                    &worktree_path.display().to_string(),
                ])
                .current_dir(&self.repo_root)
                .output()
                .map_err(|e| WorktreeError::Io(e.to_string()))?;

            if !output.status.success() {
                let _ = std::fs::remove_dir_all(&worktree_path);
            }
        }

        if delete_branch {
            let branch_name = format!("dcode-ai/{session_id}");
            let _ = Command::new("git")
                .args(["branch", "-D", &branch_name])
                .current_dir(&self.repo_root)
                .output();
        }

        Ok(())
    }

    /// List all dcode-ai worktrees.
    pub fn list_worktrees(&self) -> Vec<WorktreeInfo> {
        let worktrees_dir = self.repo_root.join(".dcode-ai").join("worktrees");
        if !worktrees_dir.exists() {
            return Vec::new();
        }

        let mut result = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&worktrees_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir()
                    && let Some(name) = path.file_name().and_then(|n| n.to_str())
                {
                    let session_id = name.to_string();
                    let branch_name = format!("dcode-ai/{session_id}");
                    let base_branch = self.current_branch().unwrap_or_else(|_| "main".into());
                    result.push(WorktreeInfo {
                        worktree_path: path,
                        branch_name,
                        base_branch,
                        session_id,
                        created_at: chrono::Utc::now(),
                    });
                }
            }
        }
        result
    }

    /// Get changed files in a worktree relative to its base branch.
    pub fn changed_files(&self, worktree_path: &Path, base_branch: &str) -> Vec<ChangedFile> {
        let output = Command::new("git")
            .args(["diff", "--name-status", base_branch])
            .current_dir(worktree_path)
            .output();

        let output = match output {
            Ok(o) if o.status.success() => o,
            _ => return Vec::new(),
        };

        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let (status, path) = line.split_once('\t')?;
                let change_type = match status.chars().next()? {
                    'A' => ChangeType::Added,
                    'M' => ChangeType::Modified,
                    'D' => ChangeType::Deleted,
                    'R' => ChangeType::Renamed,
                    _ => ChangeType::Modified,
                };
                Some(ChangedFile {
                    path: PathBuf::from(path),
                    change_type,
                })
            })
            .collect()
    }

    /// Whether a worktree holds any work: uncommitted or untracked changes,
    /// or commits its base doesn't have.
    pub fn has_work(&self, worktree_path: &Path, base_branch: &str) -> bool {
        let dirty = Command::new("git")
            .args(["status", "--porcelain", "--untracked-files=all", "--"])
            .args(OWN_WORK)
            .current_dir(worktree_path)
            .output()
            .map(|o| !o.status.success() || !o.stdout.trim_ascii().is_empty())
            // Unreadable: keep it rather than lose work.
            .unwrap_or(true);
        dirty || self.ahead_behind(worktree_path, base_branch).0 > 0
    }

    /// Get the diff for a specific file in a worktree.
    pub fn file_diff(&self, worktree_path: &Path, base_branch: &str, file_path: &Path) -> String {
        let output = Command::new("git")
            .args(["diff", base_branch, "--", &file_path.display().to_string()])
            .current_dir(worktree_path)
            .output();

        match output {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
            _ => String::new(),
        }
    }

    /// Get ahead/behind counts relative to base branch.
    pub fn ahead_behind(&self, worktree_path: &Path, base_branch: &str) -> (usize, usize) {
        let output = Command::new("git")
            .args([
                "rev-list",
                "--left-right",
                "--count",
                &format!("{base_branch}...HEAD"),
            ])
            .current_dir(worktree_path)
            .output();

        match output {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout);
                let mut parts = text.trim().split('\t');
                let behind = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                let ahead = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                (ahead, behind)
            }
            _ => (0, 0),
        }
    }

    /// Merge the worktree branch back into the base branch.
    pub fn merge_into_base(
        &self,
        session_id: &str,
        base_branch: &str,
    ) -> Result<(), WorktreeError> {
        let branch_name = format!("dcode-ai/{session_id}");

        let output = Command::new("git")
            .args(["checkout", base_branch])
            .current_dir(&self.repo_root)
            .output()
            .map_err(|e| WorktreeError::Io(e.to_string()))?;

        if !output.status.success() {
            return Err(WorktreeError::GitFailed(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        let output = Command::new("git")
            .args([
                "merge",
                "--no-ff",
                &branch_name,
                "-m",
                &format!("Merge dcode-ai session {session_id}"),
            ])
            .current_dir(&self.repo_root)
            .output()
            .map_err(|e| WorktreeError::Io(e.to_string()))?;

        if !output.status.success() {
            return Err(WorktreeError::GitFailed(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        Ok(())
    }

    /// Commit any uncommitted work in a worktree on its own branch, so a
    /// merge carries it. Returns whether a commit was made.
    pub fn commit_pending(
        &self,
        worktree_path: &Path,
        message: &str,
    ) -> Result<bool, WorktreeError> {
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(worktree_path)
                .output()
                .map_err(|e| WorktreeError::Io(e.to_string()))
        };
        let mut status_args = vec!["status", "--porcelain", "--untracked-files=all", "--"];
        status_args.extend(OWN_WORK);
        let status = git(&status_args)?;
        if status.stdout.trim_ascii().is_empty() {
            return Ok(false);
        }
        let mut add_args = vec!["add", "-A", "--"];
        add_args.extend(OWN_WORK);
        let add = git(&add_args)?;
        if !add.status.success() {
            return Err(WorktreeError::GitFailed(
                String::from_utf8_lossy(&add.stderr).trim().to_string(),
            ));
        }
        // Use the user's identity; fall back to a local one when git has none.
        let has_identity = git(&["config", "user.email"])
            .map(|o| o.status.success() && !o.stdout.trim_ascii().is_empty())
            .unwrap_or(false);
        let mut args: Vec<&str> = Vec::new();
        if !has_identity {
            args.extend([
                "-c",
                "user.name=dcode-ai",
                "-c",
                "user.email=dcode-ai@localhost",
            ]);
        }
        args.extend(["commit", "--no-verify", "-q", "-m", message]);
        let commit = git(&args)?;
        if !commit.status.success() {
            return Err(WorktreeError::GitFailed(
                String::from_utf8_lossy(&commit.stderr).trim().to_string(),
            ));
        }
        Ok(true)
    }

    /// Files changed on a worktree's branch since `base_branch` (as
    /// `M path` lines) with total lines added and removed. Commit pending
    /// work first to include it.
    pub fn branch_summary(
        &self,
        worktree_path: &Path,
        base_branch: &str,
    ) -> (Vec<String>, usize, usize) {
        let range = format!("{base_branch}...HEAD");
        let run = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(worktree_path)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                .unwrap_or_default()
        };
        let files = run(&["diff", "--name-status", &range])
            .lines()
            .filter_map(|l| {
                let (status, path) = l.split_once('\t')?;
                let path = path.rsplit('\t').next().unwrap_or(path);
                Some(format!("{} {path}", status.chars().next().unwrap_or('M')))
            })
            .collect();
        let (mut added, mut removed) = (0, 0);
        for l in run(&["diff", "--numstat", &range]).lines() {
            let mut parts = l.split('\t');
            added += parts
                .next()
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(0);
            removed += parts
                .next()
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(0);
        }
        (files, added, removed)
    }

    /// Full diff of a worktree's branch since `base_branch`.
    pub fn branch_diff(&self, worktree_path: &Path, base_branch: &str) -> String {
        Command::new("git")
            .args(["diff", &format!("{base_branch}...HEAD")])
            .current_dir(worktree_path)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default()
    }

    /// Merge a session's branch into whatever is checked out in the main
    /// repo. A conflicting merge is aborted and reported; the branch stays.
    pub fn merge_into_current(&self, session_id: &str, message: &str) -> Result<(), WorktreeError> {
        let branch_name = format!("dcode-ai/{session_id}");
        let output = Command::new("git")
            .args(["merge", "--no-ff", "--no-edit", "-m", message, &branch_name])
            .current_dir(&self.repo_root)
            .output()
            .map_err(|e| WorktreeError::Io(e.to_string()))?;
        if output.status.success() {
            return Ok(());
        }
        let mut why = format!(
            "{} — branch {branch_name} kept",
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .filter(|l| !l.starts_with("Merge with strategy") && *l != "Aborting")
                .collect::<Vec<_>>()
                .join(" ")
                .trim()
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.contains("CONFLICT") {
            let _ = Command::new("git")
                .args(["merge", "--abort"])
                .current_dir(&self.repo_root)
                .output();
            why = format!(
                "the changes conflict with {}; merge aborted, branch {branch_name} kept",
                stdout
                    .lines()
                    .filter_map(|l| l.rsplit(" in ").next().filter(|_| l.contains("CONFLICT")))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        Err(WorktreeError::GitFailed(why))
    }

    /// Prune stale worktrees that no longer have active sessions.
    pub fn prune_stale(&self) {
        let _ = Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(&self.repo_root)
            .output();
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChangedFile {
    pub path: PathBuf,
    pub change_type: ChangeType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ChangeType {
    Added,
    Modified,
    Deleted,
    Renamed,
}

impl std::fmt::Display for ChangeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChangeType::Added => write!(f, "A"),
            ChangeType::Modified => write!(f, "M"),
            ChangeType::Deleted => write!(f, "D"),
            ChangeType::Renamed => write!(f, "R"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_worktrees_discovers_dcode_ai_worktree_dirs() {
        let temp = tempfile::tempdir().expect("tempdir");
        let worktrees_dir = temp.path().join(".dcode-ai").join("worktrees");
        std::fs::create_dir_all(worktrees_dir.join("session-b")).expect("session-b");
        std::fs::create_dir_all(worktrees_dir.join("session-a")).expect("session-a");
        std::fs::write(worktrees_dir.join("not-a-dir"), "ignored").expect("file");

        let manager = WorktreeManager::new(temp.path());
        let mut worktrees = manager.list_worktrees();
        worktrees.sort_by(|left, right| left.session_id.cmp(&right.session_id));

        assert_eq!(worktrees.len(), 2);
        assert_eq!(worktrees[0].session_id, "session-a");
        assert_eq!(worktrees[0].branch_name, "dcode-ai/session-a");
        assert_eq!(worktrees[0].base_branch, "main");
        assert_eq!(worktrees[1].session_id, "session-b");
    }

    #[test]
    fn changed_files_returns_empty_for_non_git_worktree() {
        let temp = tempfile::tempdir().expect("tempdir");
        let manager = WorktreeManager::new(temp.path());

        let changed = manager.changed_files(temp.path(), "main");

        assert!(changed.is_empty());
    }
}
