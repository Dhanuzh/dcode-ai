//! Thin git helpers used by the TUI/REPL for branch display and switching.
//! Each shells out to `git` and returns `None`/`false` on any failure — these
//! are best-effort conveniences, not a git library.

use std::path::Path;

pub(crate) fn git_run(args: &[&str], cwd: Option<&Path>) -> Option<String> {
    let cwd = cwd?;
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

/// Get the current git branch name for `workspace`.
pub fn git_current_branch(workspace: &Path) -> Option<String> {
    git_run(&["rev-parse", "--abbrev-ref", "HEAD"], Some(workspace))
}

/// List local git branches for `workspace`. Current branch is marked with `*`.
pub fn git_list_branches(workspace: &Path) -> Vec<String> {
    git_run(&["branch", "--no-color"], Some(workspace))
        .map(|out| {
            out.lines()
                .map(|l| {
                    l.trim_start_matches("* ")
                        .trim_start_matches("+ ")
                        .trim()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Create a new branch `name` and check it out in `workspace`.
pub fn git_create_branch(workspace: &Path, name: &str) -> bool {
    git_run(&["checkout", "-b", name], Some(workspace)).is_some()
}

/// Switch to an existing branch `name` in `workspace`.
pub fn git_switch_branch(workspace: &Path, name: &str) -> bool {
    git_run(&["checkout", name], Some(workspace)).is_some()
}
