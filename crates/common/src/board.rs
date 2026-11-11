//! The project's task board: Jira-style cards in kanban columns, with
//! blockers, milestones and review gates. Kept in
//! `<project>/.dcode-ai/board/board.json`, shared by every session in the
//! project (the TUI's `/board` and the agent's `board_*` tools). The folder
//! ignores itself in git (its own `.gitignore`); delete that file to share
//! the board through the repository.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Backlog,
    #[default]
    Todo,
    InProgress,
    Review,
    Done,
}

impl Status {
    pub const ALL: [Status; 5] = [
        Status::Backlog,
        Status::Todo,
        Status::InProgress,
        Status::Review,
        Status::Done,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Status::Backlog => "Backlog",
            Status::Todo => "To do",
            Status::InProgress => "In progress",
            Status::Review => "Review",
            Status::Done => "Done",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Status::Backlog => "backlog",
            Status::Todo => "todo",
            Status::InProgress => "in_progress",
            Status::Review => "review",
            Status::Done => "done",
        }
    }

    /// `todo`, `to do`, `in progress`, `wip`, `doing`, `review`, `done`…
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase().replace(['-', ' '], "_");
        Some(match s.as_str() {
            "backlog" => Status::Backlog,
            "todo" | "to_do" | "ready" | "open" => Status::Todo,
            "in_progress" | "inprogress" | "wip" | "doing" | "started" => Status::InProgress,
            "review" | "in_review" => Status::Review,
            "done" | "closed" | "complete" | "completed" => Status::Done,
            _ => return None,
        })
    }

    pub fn index(self) -> usize {
        Status::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    /// The column `step` columns away (clamped).
    pub fn step(self, step: i32) -> Self {
        let i = (self.index() as i32 + step).clamp(0, Status::ALL.len() as i32 - 1);
        Status::ALL[i as usize]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    #[default]
    Medium,
    High,
    Urgent,
}

impl Priority {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "low" | "p3" => Priority::Low,
            "medium" | "med" | "normal" | "p2" => Priority::Medium,
            "high" | "p1" => Priority::High,
            "urgent" | "critical" | "p0" => Priority::Urgent,
            _ => return None,
        })
    }

    pub fn id(self) -> &'static str {
        match self {
            Priority::Low => "low",
            Priority::Medium => "medium",
            Priority::High => "high",
            Priority::Urgent => "urgent",
        }
    }

    /// Low → Medium → High → Urgent → Low.
    pub fn next(self) -> Self {
        match self {
            Priority::Low => Priority::Medium,
            Priority::Medium => Priority::High,
            Priority::High => Priority::Urgent,
            Priority::Urgent => Priority::Low,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub at: DateTime<Utc>,
    pub by: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Card {
    pub key: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default)]
    pub status: Status,
    #[serde(default)]
    pub priority: Priority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milestone: Option<String>,
    /// A review gate: the card waits in Review for a person to move it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub gate: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Note>,
    /// Personal (here only) or Jira (synced with the linked project).
    #[serde(default, skip_serializing_if = "is_personal")]
    pub space: Space,
    /// The ticket this card is in an issue tracker (Jira), once linked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<Remote>,
    /// Changed here and not sent to the tracker yet (a new card, a move).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<Push>,
    /// Its title or description changed here and the ticket doesn't have
    /// it yet.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub push_fields: bool,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
}

/// Which list a card is on: your own tasks, or the team's Jira tickets.
/// They stay apart: a personal card never goes to Jira.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Space {
    #[default]
    Personal,
    Jira,
}

impl Space {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "personal" | "mine" | "local" | "me" => Some(Space::Personal),
            "jira" | "team" | "ticket" | "tickets" => Some(Space::Jira),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Space::Personal => "Personal",
            Space::Jira => "Jira",
        }
    }
}

/// A card's ticket in the tracker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Remote {
    /// `PROJ-42`.
    pub key: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    /// The tracker's own status name ("In QA").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub status: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub issue_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reporter: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub due_date: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// When the ticket was made / last changed, as the tracker says it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub created: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub updated: String,
    /// The parent (epic or story) — `PROJ-7 Summary`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub parent: String,
    /// The ticket's other fields worth reading (`Task Type` → `Build`,
    /// `Planned End Date` → `2026-10-12`, sprint, components, …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<(String, String)>,
    /// Sub-tasks and linked tickets, one line each (`blocks PROJ-9 Title`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
    /// The newest comments, newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<RemoteComment>,
    /// How many comments the ticket has in all.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub comment_count: u32,
    /// The `updated` stamp the details above were read at: a ticket whose
    /// stamp moves gets them read again, the rest cost nothing.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail_of: String,
    /// The workflow moves the ticket can make from where it is now.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transitions: Vec<RemoteTransition>,
    /// A transition picked by hand (its id, its name, or the status it
    /// goes to), not sent yet.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub want_transition: String,
    /// Comments written here, not sent yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outbox: Vec<OutComment>,
    /// Files attached to the ticket.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<RemoteAttachment>,
    /// Attachments asked for (their ids), not downloaded yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub downloads: Vec<String>,
    /// Time logged here, not sent yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub worklogs: Vec<OutWorklog>,
    /// What someone else did to the ticket since you last opened the card
    /// ("Mamatha commented", "moved to Resolved").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub activity: String,
}

/// A saved Jira view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct JiraView {
    pub name: String,
    pub jql: String,
}

impl JiraLink {
    /// The active view's JQL, when one is chosen and still saved.
    pub fn active_view(&self) -> Option<&JiraView> {
        let name = self.view.as_deref()?;
        self.views
            .iter()
            .find(|v| v.name.eq_ignore_ascii_case(name))
    }
}

/// A file attached to a ticket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RemoteAttachment {
    pub id: String,
    pub filename: String,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub size: u64,
}

fn is_zero_u64(n: &u64) -> bool {
    *n == 0
}

/// Time to log on a ticket, not sent yet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OutWorklog {
    /// Jira's duration: `2h 30m`, `1d`.
    pub time: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub comment: String,
}

/// One workflow move a ticket can make.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RemoteTransition {
    pub id: String,
    /// The transition's own name ("Resolve Issue").
    pub name: String,
    /// The status it lands in ("Resolved").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub to: String,
}

impl RemoteTransition {
    /// `Resolve Issue → Resolved`.
    pub fn label(&self) -> String {
        if self.to.is_empty() || self.to.eq_ignore_ascii_case(&self.name) {
            self.name.clone()
        } else {
            format!("{} → {}", self.name, self.to)
        }
    }

    /// Whether `want` names it: its id, its name or the status it goes to.
    pub fn matches(&self, want: &str) -> bool {
        let w = want.trim();
        !w.is_empty()
            && (self.id == w
                || self.name.eq_ignore_ascii_case(w)
                || self.to.eq_ignore_ascii_case(w))
    }
}

/// A comment waiting to go to the ticket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OutComment {
    pub body: String,
    /// A service-desk internal note (not shown to the customer).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub internal: bool,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// A comment on a ticket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RemoteComment {
    pub author: String,
    /// The author's account id, when the tracker sends it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub author_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub at: String,
    pub body: String,
}

/// What's waiting to go to the tracker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Push {
    /// Make a ticket for it.
    Create,
    /// Move the ticket to the card's column.
    Status,
}

/// The board's link to a Jira project, through an MCP server (Atlassian's).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JiraLink {
    /// The MCP server's name in `.mcp.json`.
    pub server: String,
    pub cloud_id: String,
    /// The site (`https://acme.atlassian.net`), for links.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub site: String,
    pub project: String,
    /// What to pull (defaults to the project's open and recent issues).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jql: Option<String>,
    /// Only this person's tickets come in (their Jira name or email, as
    /// the user typed it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Values for fields the project requires on a new ticket, by field
    /// name or id (`Task Type` → `Build and Development`); the rest are
    /// taken from your latest ticket of the same type.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub fields: std::collections::BTreeMap<String, String>,
    /// Only tickets in the project's open sprints.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sprint: bool,
    /// Saved views: a name and the JQL it pulls (several projects, a
    /// team's filter, …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub views: Vec<JiraView>,
    /// The view the board shows; `None` is the default pull.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<String>,
    /// See [`Board::extra`].
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Milestone {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Board {
    pub prefix: String,
    /// The number the next card gets.
    #[serde(default = "one")]
    pub next: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub milestones: Vec<Milestone>,
    #[serde(default)]
    pub cards: Vec<Card>,
    /// Synced with a Jira project, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jira: Option<JiraLink>,
    /// Fields this version doesn't know (a newer dcode-ai wrote them), kept
    /// so saving here doesn't drop them.
    #[serde(flatten)]
    pub extra: Extra,
}

/// Unknown fields, carried through a load and save untouched — several
/// dcode-ai sessions (maybe different versions) share one board file.
pub type Extra = serde_json::Map<String, serde_json::Value>;

fn is_personal(s: &Space) -> bool {
    *s == Space::Personal
}

fn one() -> u32 {
    1
}

/// What a new card starts with.
#[derive(Debug, Clone, Default)]
pub struct NewCard {
    pub title: String,
    pub description: String,
    pub status: Option<Status>,
    pub priority: Option<Priority>,
    pub assignee: Option<String>,
    pub blocked_by: Vec<String>,
    pub milestone: Option<String>,
    pub gate: bool,
    /// Personal by default; Jira makes a ticket in the linked project.
    pub space: Space,
}

/// `<project>/.dcode-ai/board/board.json`. A sub-agent's worktree
/// (`<project>/.dcode-ai/worktrees/<id>`) uses its project's board.
pub fn board_path(workspace: &Path) -> PathBuf {
    project_root(workspace)
        .join(".dcode-ai")
        .join("board")
        .join("board.json")
}

/// Makes `dir` and, the first time, a `.gitignore` in it that ignores the
/// whole folder (so dcode-ai's own files stay out of the project's git).
pub fn ensure_ignored_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        std::fs::write(
            ignore,
            "# dcode-ai's own files: not part of the project\n*\n",
        )?;
    }
    Ok(())
}

/// The project a worktree belongs to (itself when it isn't one).
pub fn project_root(workspace: &Path) -> PathBuf {
    let parts: Vec<_> = workspace.components().collect();
    for i in (0..parts.len().saturating_sub(2)).rev() {
        if parts[i].as_os_str() == ".dcode-ai" && parts[i + 1].as_os_str() == "worktrees" {
            return parts[..i].iter().collect();
        }
    }
    workspace.to_path_buf()
}

/// The key prefix for a project: its folder's first three letters
/// (`dcode-ai` → `DCO`), `TASK` when it has none.
pub fn prefix_for(workspace: &Path) -> String {
    let name = project_root(workspace)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let p: String = name
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .take(3)
        .collect::<String>()
        .to_ascii_uppercase();
    if p.is_empty() { "TASK".into() } else { p }
}

/// A short-lived lock file around a load–change–save (another process
/// waits up to a second; a lock older than 5 s is left over and taken).
struct BoardLock(Option<PathBuf>);

impl BoardLock {
    fn take(workspace: &Path) -> Self {
        let path = board_path(workspace).with_extension("json.lock");
        if let Some(dir) = path.parent() {
            let _ = ensure_ignored_dir(dir);
        }
        for _ in 0..50 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Self(Some(path)),
                Err(_) => {
                    let stale = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age.as_secs() >= 5);
                    if stale {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
        }
        Self(None)
    }
}

impl Drop for BoardLock {
    fn drop(&mut self) {
        if let Some(p) = &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

impl Board {
    pub fn new(prefix: &str) -> Self {
        Self {
            prefix: prefix.to_string(),
            next: 1,
            milestones: Vec::new(),
            cards: Vec::new(),
            jira: None,
            extra: Extra::new(),
        }
    }

    /// The workspace's board (a new, empty one when there is none yet).
    pub fn load(workspace: &Path) -> Self {
        std::fs::read_to_string(board_path(workspace))
            .ok()
            .and_then(|s| serde_json::from_str::<Board>(&s).ok())
            .unwrap_or_else(|| Board::new(&prefix_for(workspace)))
    }

    /// Written to a temp file and renamed, so a reader never sees half.
    pub fn save(&self, workspace: &Path) -> std::io::Result<()> {
        let path = board_path(workspace);
        if let Some(dir) = path.parent() {
            ensure_ignored_dir(dir)?;
        }
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)
    }

    /// Load, change, save — the board as it is on disk now, so the TUI and
    /// the agent don't overwrite each other.
    pub fn update<T>(workspace: &Path, change: impl FnOnce(&mut Board) -> T) -> std::io::Result<T> {
        let _lock = BoardLock::take(workspace);
        // A board on disk that doesn't read (written by a newer version,
        // or damaged) is never replaced by an empty one.
        let mut board = match std::fs::read_to_string(board_path(workspace)) {
            Ok(s) => serde_json::from_str::<Board>(&s).map_err(|e| {
                std::io::Error::other(format!(
                    "{} doesn't read ({e}); not overwriting it",
                    board_path(workspace).display()
                ))
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Board::new(&prefix_for(workspace))
            }
            Err(e) => return Err(e),
        };
        let out = change(&mut board);
        board.save(workspace)?;
        Ok(out)
    }

    pub fn card(&self, key: &str) -> Option<&Card> {
        let key = key.trim();
        self.cards.iter().find(|c| c.matches(key))
    }

    pub fn card_mut(&mut self, key: &str) -> Option<&mut Card> {
        let key = key.trim();
        self.cards.iter_mut().find(|c| c.matches(key))
    }

    /// A card made here got its ticket: it takes the ticket's key
    /// (blockers pointing at it follow).
    pub fn rename(&mut self, from: &str, to: &str) {
        for c in &mut self.cards {
            if c.key == from {
                c.key = to.to_string();
            }
            for b in &mut c.blocked_by {
                if b == from {
                    *b = to.to_string();
                }
            }
        }
    }

    /// A ticket pulled from the tracker that isn't on the board yet.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_remote(
        &mut self,
        remote: Remote,
        title: &str,
        description: &str,
        status: Status,
        priority: Priority,
        assignee: Option<String>,
    ) {
        let now = Utc::now();
        self.cards.push(Card {
            key: remote.key.clone(),
            title: title.chars().take(200).collect(),
            description: description.chars().take(4000).collect(),
            status,
            priority,
            assignee,
            blocked_by: Vec::new(),
            milestone: None,
            gate: false,
            branch: None,
            notes: Vec::new(),
            space: Space::Jira,
            remote: Some(remote),
            push: None,
            push_fields: false,
            created: now,
            updated: now,
        });
    }

    /// Cards waiting to go to the tracker.
    pub fn pending(&self) -> usize {
        self.cards.iter().filter(|c| !c.unsent_nothing()).count()
    }

    /// Forgets the Jira tickets a sync brought in that have nothing waiting
    /// to go back (the next sync brings back those that still match what's
    /// pulled). Cards not yet sent, or with a move not yet sent, stay.
    /// Returns how many went.
    pub fn drop_synced(&mut self) -> usize {
        let before = self.cards.len();
        self.cards
            .retain(|c| !(c.space == Space::Jira && c.remote.is_some() && c.unsent_nothing()));
        before - self.cards.len()
    }

    /// The cards of one space.
    pub fn in_space(&self, space: Space) -> impl Iterator<Item = &Card> {
        self.cards.iter().filter(move |c| c.space == space)
    }

    /// The canonical key for `k` (`dco-3` → `DCO-3`; `3` → `DCO-3`).
    pub fn resolve_key(&self, k: &str) -> Option<String> {
        let k = k.trim().trim_start_matches('#');
        if let Ok(n) = k.parse::<u32>() {
            let key = format!("{}-{n}", self.prefix);
            return self.card(&key).map(|c| c.key.clone());
        }
        self.card(k).map(|c| c.key.clone())
    }

    /// Adds a card; returns its key.
    pub fn add(&mut self, new: NewCard, by: &str) -> String {
        let key = format!("{}-{}", self.prefix, self.next);
        self.next += 1;
        let now = Utc::now();
        let blocked_by = new
            .blocked_by
            .iter()
            .filter_map(|b| self.resolve_key(b))
            .collect();
        let milestone = new.milestone.map(|m| self.milestone_id(&m));
        let mut card = Card {
            key: key.clone(),
            title: new.title.trim().chars().take(200).collect(),
            description: new.description.trim().chars().take(4000).collect(),
            status: new.status.unwrap_or_default(),
            priority: new.priority.unwrap_or_default(),
            assignee: new.assignee.filter(|a| !a.trim().is_empty()),
            blocked_by,
            milestone,
            gate: new.gate,
            branch: None,
            notes: Vec::new(),
            space: new.space,
            remote: None,
            // A Jira card gets its ticket at the next sync.
            push: (new.space == Space::Jira).then_some(Push::Create),
            push_fields: false,
            created: now,
            updated: now,
        };
        if !by.is_empty() {
            card.notes.push(Note {
                at: now,
                by: by.to_string(),
                text: "created".into(),
            });
        }
        self.cards.push(card);
        key
    }

    /// The milestone with this id or title, made when there is none.
    pub fn milestone_id(&mut self, m: &str) -> String {
        let m = m.trim();
        if let Some(found) = self
            .milestones
            .iter()
            .find(|x| x.id.eq_ignore_ascii_case(m) || x.title.eq_ignore_ascii_case(m))
        {
            return found.id.clone();
        }
        let id = format!("M{}", self.milestones.len() + 1);
        self.milestones.push(Milestone {
            id: id.clone(),
            title: m.to_string(),
        });
        id
    }

    pub fn milestone_title(&self, id: &str) -> Option<&str> {
        self.milestones
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.title.as_str())
    }

    /// Blockers of `card` that aren't done yet.
    pub fn open_blockers<'a>(&'a self, card: &'a Card) -> Vec<&'a str> {
        card.blocked_by
            .iter()
            .filter(|b| self.card(b).is_some_and(|c| c.status != Status::Done))
            .map(String::as_str)
            .collect()
    }

    /// To do, and every blocker done: what can start now.
    pub fn ready(&self) -> Vec<&Card> {
        let mut out: Vec<&Card> = self
            .cards
            .iter()
            .filter(|c| c.status == Status::Todo && self.open_blockers(c).is_empty())
            .collect();
        out.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.created.cmp(&b.created)));
        out
    }

    pub fn column(&self, status: Status) -> Vec<&Card> {
        let mut out: Vec<&Card> = self.cards.iter().filter(|c| c.status == status).collect();
        out.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.created.cmp(&b.created)));
        out
    }

    pub fn remove(&mut self, key: &str) -> bool {
        let Some(key) = self.resolve_key(key) else {
            return false;
        };
        self.cards.retain(|c| c.key != key);
        for c in &mut self.cards {
            c.blocked_by.retain(|b| *b != key);
        }
        true
    }

    /// The board as text, for the agent and `/board list`.
    pub fn render_text(&self) -> String {
        if self.cards.is_empty() {
            return "The board is empty.".into();
        }
        let mut out = String::new();
        for status in Status::ALL {
            let cards = self.column(status);
            if cards.is_empty() {
                continue;
            }
            out.push_str(&format!("## {} ({})\n", status.label(), cards.len()));
            for c in cards {
                out.push_str(&format!("- {} [{}] {}", c.key, c.priority.id(), c.title));
                if c.space == Space::Jira {
                    out.push_str(if c.push == Some(Push::Create) {
                        " · jira (ticket being made)"
                    } else {
                        " · jira"
                    });
                }
                if let Some(a) = &c.assignee {
                    out.push_str(&format!(" · @{a}"));
                }
                let open = self.open_blockers(c);
                if !open.is_empty() {
                    out.push_str(&format!(" · blocked by {}", open.join(", ")));
                }
                if let Some(m) = c.milestone.as_deref().and_then(|m| self.milestone_title(m)) {
                    out.push_str(&format!(" · {m}"));
                }
                if c.gate {
                    out.push_str(" · review gate");
                }
                if let Some(b) = &c.branch {
                    out.push_str(&format!(" · branch {b}"));
                }
                out.push('\n');
            }
        }
        let ready: Vec<&str> = self.ready().iter().map(|c| c.key.as_str()).collect();
        if !ready.is_empty() {
            out.push_str(&format!("\nReady to start: {}\n", ready.join(", ")));
        }
        out
    }

    /// One card in full, for the agent.
    pub fn render_card(&self, c: &Card) -> String {
        let mut out = format!(
            "{} — {}\nstatus: {} · priority: {}",
            c.key,
            c.title,
            c.status.id(),
            c.priority.id()
        );
        if let Some(a) = &c.assignee {
            out.push_str(&format!(" · assignee: {a}"));
        }
        if !c.blocked_by.is_empty() {
            out.push_str(&format!("\nblocked by: {}", c.blocked_by.join(", ")));
        }
        if c.gate {
            out.push_str("\nreview gate: a person moves it to done");
        }
        if let Some(rm) = &c.remote {
            if !rm.activity.is_empty() {
                out.push_str(&format!("\n● {}", rm.activity));
            }
            if !rm.reporter.is_empty() {
                out.push_str(&format!("\nreporter: {}", rm.reporter));
            }
            if !rm.due_date.is_empty() {
                out.push_str(&format!("\ndue: {}", rm.due_date));
            }
            if !rm.labels.is_empty() {
                out.push_str(&format!("\nlabels: {}", rm.labels.join(", ")));
            }
            if !rm.issue_type.is_empty() {
                out.push_str(&format!("\ntype: {}", rm.issue_type));
            }
            if !rm.parent.is_empty() {
                out.push_str(&format!("\nparent: {}", rm.parent));
            }
            let day = |s: &str| s.get(..10).unwrap_or(s).to_string();
            if !rm.created.is_empty() || !rm.updated.is_empty() {
                out.push_str(&format!(
                    "\ncreated: {} · updated: {}",
                    day(&rm.created),
                    day(&rm.updated)
                ));
            }
            for (k, v) in &rm.fields {
                out.push_str(&format!("\n{k}: {v}"));
            }
            if !rm.links.is_empty() {
                out.push_str("\nlinks:");
                for l in &rm.links {
                    out.push_str(&format!("\n  {l}"));
                }
            }
        }
        if !c.description.is_empty() {
            out.push_str(&format!("\n\n{}", c.description));
        }
        if let Some(rm) = &c.remote
            && !rm.comments.is_empty()
        {
            out.push_str(&format!(
                "\n\ncomments ({}, newest first):",
                rm.comment_count.max(rm.comments.len() as u32)
            ));
            for cm in &rm.comments {
                let body: String = cm.body.trim().chars().take(600).collect();
                out.push_str(&format!(
                    "\n- {} {}: {}",
                    cm.at.get(..16).unwrap_or(&cm.at).replace('T', " "),
                    cm.author,
                    body.replace('\n', "\n  ")
                ));
            }
        }
        if let Some(rm) = &c.remote {
            for o in &rm.outbox {
                out.push_str(&format!(
                    "\n⟳ {} waiting to go: {}",
                    if o.internal {
                        "internal note"
                    } else {
                        "comment"
                    },
                    o.body.chars().take(200).collect::<String>()
                ));
            }
            if !rm.want_transition.is_empty() {
                out.push_str(&format!("\n⟳ moving: {}", rm.want_transition));
            }
            for w in &rm.worklogs {
                out.push_str(format!("\n⟳ logging {} {}", w.time, w.comment).trim_end());
            }
            if !rm.attachments.is_empty() {
                let list: Vec<String> = rm
                    .attachments
                    .iter()
                    .enumerate()
                    .map(|(i, a)| {
                        let size = match a.size {
                            0 => String::new(),
                            n if n >= 1 << 20 => {
                                format!(" ({:.1} MB)", n as f64 / (1u64 << 20) as f64)
                            }
                            n => format!(" ({} KB)", n.div_ceil(1024)),
                        };
                        format!("{} {}{size}", i + 1, a.filename)
                    })
                    .collect();
                out.push_str(&format!("\nattachments: {}", list.join(" · ")));
            }
            if !rm.downloads.is_empty() {
                out.push_str(&format!(
                    "\n⟳ downloading {} attachment(s)",
                    rm.downloads.len()
                ));
            }
            if !rm.transitions.is_empty() {
                let can: Vec<String> = rm
                    .transitions
                    .iter()
                    .enumerate()
                    .map(|(i, t)| format!("{} {}", i + 1, t.label()))
                    .collect();
                out.push_str(&format!("\ncan move: {}", can.join(" · ")));
            }
        }
        for n in c.notes.iter().rev().take(10).rev() {
            out.push_str(&format!(
                "\n- {} {}: {}",
                n.at.format("%Y-%m-%d %H:%M"),
                n.by,
                n.text
            ));
        }
        out
    }
}

impl Card {
    pub fn note(&mut self, by: &str, text: &str) {
        let now = Utc::now();
        self.notes.push(Note {
            at: now,
            by: by.to_string(),
            text: text.trim().chars().take(2000).collect(),
        });
        self.updated = now;
    }

    /// Nothing here waits to go to the tracker.
    pub fn unsent_nothing(&self) -> bool {
        self.push.is_none()
            && !self.push_fields
            && self.remote.as_ref().is_none_or(|r| {
                r.want_transition.is_empty()
                    && r.outbox.is_empty()
                    && r.worklogs.is_empty()
                    && r.downloads.is_empty()
            })
    }

    /// Its title or description changed: the ticket gets them next sync
    /// (a card still to be made goes with them anyway).
    pub fn edited(&mut self) {
        self.updated = Utc::now();
        if self.remote.is_some() && self.push != Some(Push::Create) {
            self.push_fields = true;
        }
    }

    /// Move it; a note says who did.
    pub fn set_status(&mut self, status: Status, by: &str) {
        if self.status != status {
            let text = format!("{} → {}", self.status.label(), status.label());
            self.status = status;
            self.note(by, &text);
            // A ticket follows the card (a card still to be made goes as is).
            if self.remote.is_some() && self.push != Some(Push::Create) {
                self.push = Some(Push::Status);
            }
        }
    }

    fn matches(&self, key: &str) -> bool {
        self.key.eq_ignore_ascii_case(key)
            || self
                .remote
                .as_ref()
                .is_some_and(|r| r.key.eq_ignore_ascii_case(key))
    }

    /// The key shown: the ticket's when it has one.
    pub fn shown_key(&self) -> &str {
        self.remote.as_ref().map_or(&self.key, |r| &r.key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changing_the_jira_filter_keeps_unsent_work() {
        let mut b = Board::new("DCO");
        let remote = |k: &str| Remote {
            key: k.into(),
            status: "To Do".into(),
            ..Default::default()
        };
        b.insert_remote(
            remote("BSI-1"),
            "synced",
            "",
            Status::Todo,
            Priority::Medium,
            None,
        );
        b.insert_remote(
            remote("BSI-2"),
            "moved here",
            "",
            Status::Todo,
            Priority::Medium,
            None,
        );
        b.card_mut("BSI-2").unwrap().push = Some(Push::Status);
        b.add(
            NewCard {
                title: "no ticket yet".into(),
                space: Space::Jira,
                ..Default::default()
            },
            "",
        );
        b.add(
            NewCard {
                title: "personal".into(),
                ..Default::default()
            },
            "",
        );
        assert_eq!(b.drop_synced(), 1);
        assert!(b.card("BSI-1").is_none());
        assert!(b.card("BSI-2").is_some());
        assert_eq!(b.cards.len(), 3);
        // Boards saved before the assignee filter still load.
        let l: JiraLink =
            serde_json::from_str(r#"{"server":"atlassian","cloud_id":"c","project":"BSI"}"#)
                .unwrap();
        assert_eq!(l.assignee, None);
    }

    #[test]
    fn cards_get_keys_and_blockers_decide_what_is_ready() {
        let mut b = Board::new("DCO");
        let a = b.add(
            NewCard {
                title: "Login page".into(),
                ..Default::default()
            },
            "agent",
        );
        let c = b.add(
            NewCard {
                title: "Checkout".into(),
                blocked_by: vec!["1".into()],
                priority: Some(Priority::High),
                milestone: Some("Launch".into()),
                gate: true,
                ..Default::default()
            },
            "agent",
        );
        assert_eq!((a.as_str(), c.as_str()), ("DCO-1", "DCO-2"));
        assert_eq!(b.card("dco-2").unwrap().blocked_by, vec!["DCO-1"]);
        let ready: Vec<_> = b.ready().iter().map(|c| c.key.clone()).collect();
        assert_eq!(ready, vec!["DCO-1"]);
        b.card_mut("DCO-1")
            .unwrap()
            .set_status(Status::Done, "Arun");
        let ready: Vec<_> = b.ready().iter().map(|c| c.key.clone()).collect();
        assert_eq!(ready, vec!["DCO-2"]);
        let text = b.render_text();
        assert!(text.contains("DCO-2 [high] Checkout"), "{text}");
        assert!(text.contains("Launch") && text.contains("review gate"));
        assert!(b.remove("DCO-1"));
        assert!(b.card("DCO-2").unwrap().blocked_by.is_empty());
    }

    #[test]
    fn fields_from_another_version_survive_a_save() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let json = r#"{"prefix":"BSI","next":1,"cards":[],"future":7,
            "jira":{"server":"a","cloud_id":"c","project":"BSI","assignee":"me","filter2":"x"}}"#;
        let path = board_path(ws);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, json).unwrap();
        Board::update(ws, |b| b.next = 2).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["future"], 7);
        assert_eq!(v["jira"]["filter2"], "x");
        assert_eq!(v["jira"]["assignee"], "me");
    }

    #[test]
    fn saves_and_loads_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("my-app");
        std::fs::create_dir_all(&ws).unwrap();
        let key = Board::update(&ws, |b| {
            b.add(
                NewCard {
                    title: "First".into(),
                    ..Default::default()
                },
                "you",
            )
        })
        .unwrap();
        assert_eq!(key, "MYA-1");
        let b = Board::load(&ws);
        assert_eq!(b.cards.len(), 1);
        assert_eq!(b.next, 2);
        // The folder keeps itself out of git.
        let ignore = ws.join(".dcode-ai/board/.gitignore");
        assert!(std::fs::read_to_string(ignore).unwrap().contains('*'));
    }

    #[test]
    fn a_worktree_uses_its_projects_board() {
        let p = Path::new("/home/me/shop/.dcode-ai/worktrees/s-123");
        assert_eq!(
            board_path(p),
            Path::new("/home/me/shop/.dcode-ai/board/board.json")
        );
        assert_eq!(prefix_for(p), "SHO");
        assert_eq!(
            project_root(Path::new("/home/me/shop")),
            Path::new("/home/me/shop")
        );
    }

    #[test]
    fn statuses_and_priorities_parse_loosely() {
        assert_eq!(Status::parse("In Progress"), Some(Status::InProgress));
        assert_eq!(Status::parse("wip"), Some(Status::InProgress));
        assert_eq!(Status::Todo.step(1), Status::InProgress);
        assert_eq!(Status::Done.step(1), Status::Done);
        assert_eq!(Priority::parse("P0"), Some(Priority::Urgent));
        assert_eq!(Priority::Urgent.next(), Priority::Low);
    }
}
