//! Git integration for autonomous research
//!
//! Handles branch creation, commits, and revert operations for experiments.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tokio::process::Command;

/// Git manager for autonomous research workflows
pub struct GitManager {
    repo_path: PathBuf,
}

impl GitManager {
    /// Create a new Git manager for the given repository
    pub fn new(repo_path: impl AsRef<Path>) -> Self {
        Self {
            repo_path: repo_path.as_ref().to_path_buf(),
        }
    }

    /// Run a git command and return the output
    async fn run(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("git")
            .current_dir(&self.repo_path)
            .args(args)
            .output()
            .await
            .context("Failed to run git command")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Git command failed: {}", stderr);
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Get the current branch name
    pub async fn current_branch(&self) -> Result<String> {
        let output = self.run(&["rev-parse", "--abbrev-ref", "HEAD"]).await?;
        Ok(output.trim().to_string())
    }

    /// Get the current commit hash (short form)
    pub async fn current_commit(&self) -> Result<String> {
        let output = self.run(&["rev-parse", "--short", "HEAD"]).await?;
        Ok(output.trim().to_string())
    }

    /// Get the full current commit hash
    pub async fn current_commit_full(&self) -> Result<String> {
        let output = self.run(&["rev-parse", "HEAD"]).await?;
        Ok(output.trim().to_string())
    }

    /// Create a new branch
    pub async fn create_branch(&self, name: &str) -> Result<()> {
        self.run(&["checkout", "-b", name]).await?;
        tracing::info!("Created branch: {}", name);
        Ok(())
    }

    /// Checkout an existing branch
    pub async fn checkout(&self, branch: &str) -> Result<()> {
        self.run(&["checkout", branch]).await?;
        Ok(())
    }

    /// Switch to a branch (newer git)
    pub async fn switch(&self, branch: &str) -> Result<()> {
        self.run(&["switch", branch]).await?;
        Ok(())
    }

    /// Create and switch to a new branch
    pub async fn create_and_switch(&self, branch: &str) -> Result<()> {
        // Try switch first (newer git), fall back to checkout
        if self.run(&["switch", "-c", branch]).await.is_err() {
            self.run(&["checkout", "-b", branch]).await?;
        }
        tracing::info!("Created and switched to branch: {}", branch);
        Ok(())
    }

    /// Commit staged changes with a message
    pub async fn commit(&self, message: &str) -> Result<String> {
        // Stage all changes
        self.run(&["add", "-A"]).await?;

        // Check if there are changes to commit
        let status = self.run(&["status", "--porcelain"]).await?;
        if status.trim().is_empty() {
            anyhow::bail!("No changes to commit");
        }

        // Commit
        self.run(&["commit", "-m", message]).await?;

        // Return the new commit hash
        let commit = self.current_commit().await?;
        tracing::info!("Committed: {} - {}", commit, message);
        Ok(commit)
    }

    /// Get the commit message for a given commit
    pub async fn commit_message(&self, commit: &str) -> Result<String> {
        let output = self.run(&["log", "-1", "--format=%B", commit]).await?;
        Ok(output.trim().to_string())
    }

    /// Reset to a specific commit (keeping changes)
    pub async fn reset_soft(&self, commit: &str) -> Result<()> {
        self.run(&["reset", "--soft", commit]).await?;
        tracing::info!("Soft reset to: {}", commit);
        Ok(())
    }

    /// Reset to a specific commit (discarding changes)
    pub async fn reset_hard(&self, commit: &str) -> Result<()> {
        self.run(&["reset", "--hard", commit]).await?;
        tracing::info!("Hard reset to: {}", commit);
        Ok(())
    }

    /// Get the diff between two commits
    pub async fn diff(&self, from: &str, to: &str) -> Result<String> {
        let output = self.run(&["diff", from, to]).await?;
        Ok(output)
    }

    /// Show files changed in a commit
    pub async fn changed_files(&self, commit: &str) -> Result<Vec<String>> {
        let output = self
            .run(&["diff-tree", "--no-commit-id", "--name-only", "-r", commit])
            .await?;
        Ok(output
            .lines()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .collect())
    }

    /// Check if there are uncommitted changes
    pub async fn is_dirty(&self) -> Result<bool> {
        let status = self.run(&["status", "--porcelain"]).await?;
        Ok(!status.trim().is_empty())
    }

    /// Discard local changes to a file
    pub async fn checkout_file(&self, file: &Path) -> Result<()> {
        let file_str = file.to_string_lossy();
        self.run(&["checkout", "--", &file_str]).await?;
        Ok(())
    }

    /// Get the log of commits
    pub async fn log(&self, count: usize) -> Result<Vec<CommitInfo>> {
        let output = self
            .run(&["log", &format!("-{}", count), "--format=%H|%s|%ci"])
            .await?;

        let commits = output
            .lines()
            .filter_map(|line| {
                let parts: Vec<&str> = line.splitn(3, '|').collect();
                if parts.len() >= 3 {
                    Some(CommitInfo {
                        hash: parts[0].to_string(),
                        message: parts[1].to_string(),
                        date: parts[2].to_string(),
                    })
                } else {
                    None
                }
            })
            .collect();

        Ok(commits)
    }

    /// Create a worktree for parallel experiments
    pub async fn create_worktree(
        &self,
        branch: &str,
        path: &Path,
        start_commit: Option<&str>,
    ) -> Result<()> {
        let mut args = vec!["worktree", "add", "-b", branch, path.to_str().unwrap_or("")];

        if let Some(commit) = start_commit {
            args.push(commit);
        }

        self.run(&args).await?;
        tracing::info!(
            "Created worktree at: {} on branch: {}",
            path.display(),
            branch
        );
        Ok(())
    }

    /// List worktrees
    pub async fn list_worktrees(&self) -> Result<Vec<WorktreeInfo>> {
        let output = self.run(&["worktree", "list", "--porcelain"]).await?;

        let mut worktrees = Vec::new();
        let mut current: Option<WorktreeInfo> = None;

        for line in output.lines() {
            if line.starts_with("worktree ") {
                if let Some(w) = current.take() {
                    worktrees.push(w);
                }
                let path = line.trim_start_matches("worktree ");
                current = Some(WorktreeInfo {
                    path: PathBuf::from(path),
                    branch: String::new(),
                    head: String::new(),
                });
            } else if let Some(ref mut w) = current {
                if line.starts_with("branch ") {
                    w.branch = line.trim_start_matches("branch ").to_string();
                } else if line.starts_with("HEAD ") {
                    w.head = line.trim_start_matches("HEAD ").to_string();
                }
            }
        }

        if let Some(w) = current {
            worktrees.push(w);
        }

        Ok(worktrees)
    }

    /// Remove a worktree
    pub async fn remove_worktree(&self, path: &Path, force: bool) -> Result<()> {
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(path.to_str().unwrap_or(""));

        self.run(&args).await?;
        tracing::info!("Removed worktree: {}", path.display());
        Ok(())
    }

    /// Stash changes
    pub async fn stash(&self, message: Option<&str>) -> Result<()> {
        match message {
            Some(msg) => {
                self.run(&["stash", "push", "-m", msg]).await?;
            }
            None => {
                self.run(&["stash"]).await?;
            }
        }
        Ok(())
    }

    /// Apply stashed changes
    pub async fn stash_pop(&self) -> Result<()> {
        self.run(&["stash", "pop"]).await?;
        Ok(())
    }
}

/// Information about a git commit
#[derive(Debug, Clone)]
pub struct CommitInfo {
    pub hash: String,
    pub message: String,
    pub date: String,
}

/// Information about a worktree
#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub branch: String,
    pub head: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn init_test_repo() -> Result<(TempDir, GitManager)> {
        let temp = TempDir::new()?;

        // Initialize git repo
        Command::new("git")
            .current_dir(temp.path())
            .args(["init"])
            .output()
            .await
            .expect("git init failed");

        Command::new("git")
            .current_dir(temp.path())
            .args(["config", "user.email", "test@test.com"])
            .output()
            .await
            .expect("git config failed");

        Command::new("git")
            .current_dir(temp.path())
            .args(["config", "user.name", "Test"])
            .output()
            .await
            .expect("git config failed");

        // Add an initial commit so HEAD is valid
        Command::new("git")
            .current_dir(temp.path())
            .args(["commit", "--allow-empty", "-m", "initial"])
            .output()
            .await
            .expect("git commit failed");

        let manager = GitManager::new(temp.path());
        Ok((temp, manager))
    }

    #[tokio::test]
    async fn test_current_branch() {
        let (_temp, manager) = init_test_repo().await.unwrap();
        let branch = manager.current_branch().await.unwrap();
        // git init defaults to "master" or "main" depending on version
        assert!(
            branch == "master" || branch == "main",
            "expected master or main, got {branch}"
        );
    }

    #[tokio::test]
    async fn test_commit() {
        let (_temp, manager) = init_test_repo().await.unwrap();

        // Create a file
        std::fs::write(manager.repo_path.join("test.txt"), "hello").unwrap();

        // Commit
        let commit = manager.commit("Initial commit").await.unwrap();
        assert_eq!(commit.len(), 7); // Short hash
    }

    #[tokio::test]
    async fn test_is_dirty() {
        let (_temp, manager) = init_test_repo().await.unwrap();

        assert!(!manager.is_dirty().await.unwrap());

        std::fs::write(manager.repo_path.join("test.txt"), "hello").unwrap();

        assert!(manager.is_dirty().await.unwrap());
    }
}
