//! The swarm board: a shared message board for a lead agent and its
//! sub-agents. Workers claim work ("taking: auth tests"), share findings,
//! ask each other questions and say when they're done, so they don't
//! duplicate work. One file per lead session (append-only JSON lines) at
//! `<project>/.dcode-ai/swarm/<lead-session-id>.jsonl`, so every sub-agent
//! (in its own worktree) and the terminal's dashboard read the same board.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What a post is about.
pub const KINDS: [&str; 6] = ["claim", "finding", "question", "answer", "done", "note"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Post {
    /// RFC 3339 time it was posted.
    pub at: String,
    /// Who posted: `lead`, a sub-agent's short id, or `workflow`.
    pub who: String,
    /// One of [`KINDS`].
    pub kind: String,
    pub text: String,
}

/// The board file of lead session `lead_id`.
pub fn board_path(workspace: &Path, lead_id: &str) -> PathBuf {
    crate::board::project_root(workspace)
        .join(".dcode-ai")
        .join("swarm")
        .join(format!("{lead_id}.jsonl"))
}

/// A kind from what the model wrote (unknown → `note`).
pub fn normalize_kind(kind: &str) -> &'static str {
    let k = kind.trim().to_ascii_lowercase();
    KINDS.iter().copied().find(|x| *x == k).unwrap_or("note")
}

/// Adds a post to the board (creating it). Text is kept to one paragraph
/// of at most 1 000 characters.
pub fn append(path: &Path, who: &str, kind: &str, text: &str) -> std::io::Result<Post> {
    let text: String = text.trim().chars().take(1000).collect();
    let post = Post {
        at: chrono::Utc::now().to_rfc3339(),
        who: who.trim().chars().take(40).collect(),
        kind: normalize_kind(kind).to_string(),
        text,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(
        f,
        "{}",
        serde_json::to_string(&post).map_err(std::io::Error::other)?
    )?;
    Ok(post)
}

/// Every post on the board, oldest first (a missing board is empty; broken
/// lines are skipped).
pub fn load(path: &Path) -> Vec<Post> {
    std::fs::read_to_string(path)
        .map(|s| {
            s.lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// The marker a kind gets on screen.
pub fn icon(kind: &str) -> &'static str {
    match kind {
        "claim" => "⚑",
        "finding" => "◆",
        "question" => "?",
        "answer" => "↳",
        "done" => "✓",
        _ => "·",
    }
}

/// `HH:MM who ⚑ text` for a post (local time).
pub fn one_line(p: &Post) -> String {
    let t = chrono::DateTime::parse_from_rfc3339(&p.at)
        .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_default();
    format!("{t} {} {} {}", p.who, icon(&p.kind), p.text)
}

/// Open claims: a `claim` by someone who hasn't posted `done` since.
pub fn open_claims(posts: &[Post]) -> Vec<&Post> {
    posts
        .iter()
        .enumerate()
        .filter(|(i, p)| {
            p.kind == "claim"
                && !posts[i + 1..]
                    .iter()
                    .any(|q| q.who == p.who && q.kind == "done")
        })
        .map(|(_, p)| p)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posts_round_trip_and_claims_close_on_done() {
        let dir = std::env::temp_dir().join(format!("dcode-swarm-{}", std::process::id()));
        let path = dir.join("lead.jsonl");
        let _ = std::fs::remove_file(&path);
        append(&path, "a1", "claim", "taking: auth tests").unwrap();
        append(&path, "b2", "CLAIM", "taking: docs").unwrap();
        append(&path, "a1", "finding", "login uses < not <=").unwrap();
        append(&path, "a1", "done", "auth tests pass").unwrap();
        append(&path, "b2", "weird", "x").unwrap();
        let posts = load(&path);
        assert_eq!(posts.len(), 5);
        assert_eq!(posts[1].kind, "claim");
        assert_eq!(posts[4].kind, "note");
        let open = open_claims(&posts);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].who, "b2");
        assert!(one_line(&posts[0]).contains("a1 ⚑ taking: auth tests"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_board_is_empty() {
        assert!(load(Path::new("/nonexistent/dcode/board.jsonl")).is_empty());
    }
}
