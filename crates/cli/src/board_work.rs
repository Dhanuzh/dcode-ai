//! `/board work <KEY>`: start on a card — its branch, its ticket moved to
//! In progress, and the agent handed the card in full (description, fields,
//! links, comments) with what done means: tests, a commit, a pull request
//! titled after the ticket, and the PR's link posted back on the ticket.

use std::path::Path;
use std::process::Command;

use dcode_ai_common::board::Card;

/// `bsi-513-ncd-lcd-webpart`: the card's key and a slug of its title.
pub fn branch_name(card: &Card) -> String {
    let mut slug = String::new();
    for c in card.title.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
        if slug.len() >= 40 {
            break;
        }
    }
    let slug = slug.trim_end_matches('-');
    let key = card.shown_key().to_ascii_lowercase();
    if slug.is_empty() {
        key
    } else {
        format!("{key}-{slug}")
    }
}

fn git(ws: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(ws)
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Switches this repo to `branch` (made from the current one if new).
/// Refuses when tracked files have changes — they'd ride along onto the
/// ticket's branch. Returns what happened, in words.
pub fn switch_branch(ws: &Path, branch: &str) -> Result<String, String> {
    git(ws, &["rev-parse", "--is-inside-work-tree"])
        .map_err(|_| "this folder isn't a git repository".to_string())?;
    let current = git(ws, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    if current == branch {
        return Ok(format!("already on {branch}"));
    }
    let dirty = git(ws, &["status", "--porcelain", "--untracked-files=no"])?;
    if !dirty.is_empty() {
        let n = dirty.lines().count();
        return Err(format!(
            "{n} changed file(s) on {current} — commit or stash them first, or `/board work <KEY> worktree` to work in a separate worktree"
        ));
    }
    let exists = git(
        ws,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok();
    if exists {
        git(ws, &["switch", branch])?;
        Ok(format!("switched to {branch}"))
    } else {
        git(ws, &["switch", "-c", branch])?;
        Ok(format!("made {branch} from {current}"))
    }
}

/// The instructions for the agent; the card in full follows them.
pub fn work_prompt(key: &str, branch: &str, worktree: bool, jira: bool) -> String {
    let place = if worktree {
        format!(
            "Do the work in a sub-agent with its own worktree: `spawn_subagent` with this card as the task, on a branch named `{branch}`, and the steps below as what done means. Wait for it, then finish the board and ticket steps yourself."
        )
    } else {
        format!("You're on branch `{branch}`, made for this card. Work here.")
    };
    let ticket = if jira {
        format!(
            " Then `board_update` {key} with `status: review`, `branch: {branch}`, a `note` saying what changed, and `comment` set to the pull request's link and a two-line summary — it's posted on the Jira ticket."
        )
    } else {
        format!(
            " Then `board_update` {key} with `status: review`, `branch: {branch}` and a `note` with the pull request's link."
        )
    };
    format!(
        "Work this card from the project's task board until it's ready for review. {place}\n\
1. Read the card below (description, fields, linked tickets, comments) and the code it touches. If something essential is unclear, ask before writing code.\n\
2. Implement it, with tests where the project has them; run the project's checks.\n\
3. Commit with a message starting `{key}: `, push the branch, and open a pull request titled `{key}: <the card's title>` (`gh pr create`) whose body links the ticket.{ticket}\n\
4. Tell the user what you did, the PR link, and anything left for them.\n\nThe card:\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use dcode_ai_common::board::{Board, NewCard};

    #[test]
    fn the_branch_is_named_after_the_card() {
        let mut b = Board::new("BSI");
        let k = b.add(
            NewCard {
                title: "NCD & LCD: webpart for SharePoint (v2)!".into(),
                ..Default::default()
            },
            "you",
        );
        assert_eq!(
            branch_name(b.card(&k).unwrap()),
            "bsi-1-ncd-lcd-webpart-for-sharepoint-v2"
        );
    }

    #[test]
    fn a_dirty_tree_is_not_carried_onto_the_ticket_branch() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let run = |args: &[&str]| git(ws, args).unwrap();
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(ws.join("a.txt"), "1").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);
        assert_eq!(
            switch_branch(ws, "bsi-1-x").unwrap(),
            "made bsi-1-x from main"
        );
        assert_eq!(switch_branch(ws, "bsi-1-x").unwrap(), "already on bsi-1-x");
        run(&["switch", "-q", "main"]);
        std::fs::write(ws.join("a.txt"), "2").unwrap();
        let e = switch_branch(ws, "bsi-1-x").unwrap_err();
        assert!(e.contains("1 changed file(s) on main"), "{e}");
        run(&["checkout", "--", "a.txt"]);
        assert_eq!(switch_branch(ws, "bsi-1-x").unwrap(), "switched to bsi-1-x");
    }

    #[test]
    fn the_prompt_asks_for_the_pr_link_on_the_ticket() {
        let p = work_prompt("BSI-513", "bsi-513-x", false, true);
        assert!(p.contains("`comment` set to the pull request's link"));
        assert!(p.contains("titled `BSI-513: <the card's title>`"));
        let p = work_prompt("SHO-2", "sho-2-x", true, false);
        assert!(p.contains("spawn_subagent"));
        assert!(!p.contains("`comment`"));
    }
}
