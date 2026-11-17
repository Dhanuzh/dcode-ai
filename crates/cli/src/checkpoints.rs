//! Named checkpoints of the whole working tree — like a `git stash` that
//! leaves everything in place. A checkpoint is a commit object made from a
//! temporary index (tracked and untracked files, `.gitignore` respected),
//! kept under `refs/dcode-ai/checkpoints/`; your branch, index and files
//! are not touched. Restoring puts the tree back exactly: changed files
//! restored, files added since removed — after saving the current state as
//! a checkpoint of its own, so a restore can be undone.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq)]
pub struct Checkpoint {
    pub n: usize,
    pub name: String,
    pub commit: String,
    pub when: String,
}

fn git(ws: &Path, args: &[&str], index: Option<&Path>) -> Result<String, String> {
    let mut c = Command::new("git");
    c.current_dir(ws).args(args);
    if let Some(i) = index {
        c.env("GIT_INDEX_FILE", i);
    }
    let o = c.output().map_err(|e| format!("git: {e}"))?;
    if !o.status.success() {
        return Err(format!(
            "git {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&o.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn temp_index(ws: &Path) -> Result<PathBuf, String> {
    let git_dir = git(ws, &["rev-parse", "--absolute-git-dir"], None)?;
    Ok(PathBuf::from(git_dir).join(format!("dcode-ai-checkpoint-{}.index", std::process::id())))
}

/// The tree as it is now, as a commit object (not on any branch).
fn snapshot(ws: &Path, message: &str) -> Result<String, String> {
    git(ws, &["rev-parse", "--is-inside-work-tree"], None)
        .map_err(|_| "checkpoints need a git repository (git init)".to_string())?;
    let index = temp_index(ws)?;
    let _ = std::fs::remove_file(&index);
    let r = (|| {
        git(ws, &["add", "-A", "."], Some(&index))?;
        let tree = git(ws, &["write-tree"], Some(&index))?;
        let mut args = vec!["commit-tree", tree.as_str(), "-m", message];
        let head = git(ws, &["rev-parse", "--verify", "-q", "HEAD"], None).ok();
        if let Some(h) = &head {
            args.push("-p");
            args.push(h);
        }
        git(ws, &args, None)
    })();
    let _ = std::fs::remove_file(&index);
    r
}

/// Saved checkpoints, oldest first.
pub fn list(ws: &Path) -> Result<Vec<Checkpoint>, String> {
    let out = git(
        ws,
        &[
            "for-each-ref",
            "--sort=refname",
            "--format=%(refname:lstrip=3)\t%(objectname)\t%(contents:subject)\t%(creatordate:relative)",
            "refs/dcode-ai/checkpoints/",
        ],
        None,
    )?;
    let mut v: Vec<Checkpoint> = out
        .lines()
        .filter_map(|l| {
            let mut p = l.splitn(4, '\t');
            let n: usize = p.next()?.parse().ok()?;
            Some(Checkpoint {
                n,
                commit: p.next()?.to_string(),
                name: p.next()?.to_string(),
                when: p.next().unwrap_or("").to_string(),
            })
        })
        .collect();
    v.sort_by_key(|c| c.n);
    Ok(v)
}

/// Takes a checkpoint; returns it.
pub fn take(ws: &Path, name: &str) -> Result<Checkpoint, String> {
    let name = if name.trim().is_empty() {
        chrono::Local::now().format("checkpoint %H:%M").to_string()
    } else {
        name.trim().to_string()
    };
    let commit = snapshot(ws, &name)?;
    let n = list(ws)?.last().map(|c| c.n + 1).unwrap_or(1);
    git(
        ws,
        &[
            "update-ref",
            &format!("refs/dcode-ai/checkpoints/{n:05}"),
            &commit,
        ],
        None,
    )?;
    Ok(Checkpoint {
        n,
        name,
        commit,
        when: "now".into(),
    })
}

/// Puts the working tree back to checkpoint `n`; returns (files restored,
/// files removed, the checkpoint saved of the state before).
pub fn restore(ws: &Path, n: usize) -> Result<(usize, usize, Checkpoint), String> {
    let all = list(ws)?;
    let cp = all
        .iter()
        .find(|c| c.n == n)
        .ok_or_else(|| format!("no checkpoint {n} — /checkpoint list"))?
        .clone();
    let before = take(ws, &format!("before restoring {n}"))?;
    // Files there now but not in the checkpoint go; the rest come back.
    let added = git(
        ws,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "--diff-filter=A",
            &cp.commit,
            &before.commit,
        ],
        None,
    )?;
    let mut removed = 0;
    for f in added.lines().filter(|l| !l.is_empty()) {
        if std::fs::remove_file(ws.join(f)).is_ok() {
            removed += 1;
        }
    }
    let changed = git(
        ws,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            &before.commit,
            &cp.commit,
        ],
        None,
    )?
    .lines()
    .filter(|l| !l.is_empty())
    .count();
    let index = temp_index(ws)?;
    let _ = std::fs::remove_file(&index);
    let r = git(ws, &["read-tree", &cp.commit], Some(&index))
        .and_then(|_| git(ws, &["checkout-index", "-a", "-f"], Some(&index)));
    let _ = std::fs::remove_file(&index);
    r?;
    Ok((changed.saturating_sub(removed), removed, before))
}

/// Deletes checkpoint `n`.
pub fn drop(ws: &Path, n: usize) -> Result<(), String> {
    git(
        ws,
        &[
            "update-ref",
            "-d",
            &format!("refs/dcode-ai/checkpoints/{n:05}"),
        ],
        None,
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let g = |a: &[&str]| {
            assert!(
                Command::new("git")
                    .current_dir(d.path())
                    .args(a)
                    .output()
                    .unwrap()
                    .status
                    .success()
            )
        };
        g(&["init", "-q"]);
        g(&["config", "user.email", "t@example.com"]);
        g(&["config", "user.name", "t"]);
        std::fs::write(d.path().join("a.txt"), "one").unwrap();
        g(&["add", "."]);
        g(&["commit", "-qm", "first"]);
        d
    }

    #[test]
    fn take_change_and_restore() {
        let d = repo();
        let ws = d.path();
        std::fs::write(ws.join("a.txt"), "two").unwrap();
        std::fs::write(ws.join("new.txt"), "untracked").unwrap();
        let cp = take(ws, "midway").unwrap();
        assert_eq!(cp.n, 1);
        // Nothing moved: branch, index and files as they were.
        assert_eq!(std::fs::read_to_string(ws.join("a.txt")).unwrap(), "two");
        let status = git(ws, &["status", "--porcelain"], None).unwrap();
        assert!(
            status.contains("M a.txt") && status.contains("?? new.txt"),
            "{status}"
        );
        // Then things change…
        std::fs::write(ws.join("a.txt"), "three").unwrap();
        std::fs::remove_file(ws.join("new.txt")).unwrap();
        std::fs::write(ws.join("later.txt"), "x").unwrap();
        let (changed, removed, before) = restore(ws, 1).unwrap();
        assert_eq!(std::fs::read_to_string(ws.join("a.txt")).unwrap(), "two");
        assert_eq!(
            std::fs::read_to_string(ws.join("new.txt")).unwrap(),
            "untracked"
        );
        assert!(!ws.join("later.txt").exists());
        assert_eq!(removed, 1);
        assert!(changed >= 1);
        // …and the restore can itself be undone.
        assert_eq!(before.n, 2);
        restore(ws, 2).unwrap();
        assert_eq!(std::fs::read_to_string(ws.join("a.txt")).unwrap(), "three");
        assert!(ws.join("later.txt").exists());
        let names: Vec<String> = list(ws).unwrap().into_iter().map(|c| c.name).collect();
        assert_eq!(names[0], "midway");
        drop(ws, 1).unwrap();
        assert_eq!(list(ws).unwrap().len(), 2);
    }

    #[test]
    fn outside_git_says_so() {
        let d = tempfile::tempdir().unwrap();
        assert!(take(d.path(), "x").unwrap_err().contains("git"));
    }
}
