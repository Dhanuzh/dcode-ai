//! The board's Jira space, kept in step with a Jira project through an MCP
//! server (Atlassian's: `https://mcp.atlassian.com/v1/mcp`, signed in with
//! `dcode-ai mcp login <name>`). dcode-ai calls the server's tools itself —
//! no model in between, no tokens spent:
//!
//! - new Jira cards become tickets (`createJiraIssue`), then take the
//!   ticket's key;
//! - a card moved here moves its ticket (`transitionJiraIssue`, the
//!   transition whose target fits the column);
//! - the project's open and recent tickets come in (`searchJiraIssuesUsingJql`).
//!
//! Personal cards never go to Jira.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dcode_ai_common::board::{
    Board, JiraLink, OutComment, Priority, Push, Remote, RemoteTransition, Space, Status,
};
use dcode_ai_common::config::McpServerConfig;
use dcode_ai_core::tools::mcp::{McpCallToolResult, McpConnectionManager};
use serde_json::{Value, json};

mod parse;
pub use parse::*;
mod fields;
use fields::*;
mod transitions;
use transitions::*;
mod report;
pub use report::*;

/// The Atlassian MCP server's tools we use.
const SEARCH: &str = "searchJiraIssuesUsingJql";
const CREATE: &str = "createJiraIssue";
const TRANSITIONS: &str = "getTransitionsForJiraIssue";
const TRANSITION: &str = "transitionJiraIssue";
const RESOURCES: &str = "getAccessibleAtlassianResources";
const ISSUE_TYPES: &str = "getJiraProjectIssueTypesMetadata";
const ISSUE_TYPE_FIELDS: &str = "getJiraIssueTypeMetaWithFields";
const EDIT: &str = "editJiraIssue";
const USERINFO: &str = "atlassianUserInfo";
const GET_ISSUE: &str = "getJiraIssue";
const COMMENTS: &str = "listJiraIssueComments";
const COMMENT: &str = "addOrEditJiraIssueComment";
const WORKLOG: &str = "addOrEditJiraIssueWorklog";
const DOWNLOAD: &str = "downloadJiraIssueAttachment";

/// Calls an MCP server's tools (the real server, or a fake in tests).
#[async_trait::async_trait]
pub trait Mcp: Send + Sync {
    async fn tools(&self) -> Result<Vec<String>, String>;
    /// The tool's text result; `Err` when it failed.
    async fn call(&self, tool: &str, args: Value) -> Result<String, String>;
}

/// A server from the session's MCP configuration.
pub struct ServerMcp {
    manager: Arc<McpConnectionManager>,
    server: McpServerConfig,
}

impl ServerMcp {
    pub fn new(manager: Arc<McpConnectionManager>, server: McpServerConfig) -> Self {
        Self { manager, server }
    }
}

#[async_trait::async_trait]
impl Mcp for ServerMcp {
    async fn tools(&self) -> Result<Vec<String>, String> {
        let client = self.manager.get_or_connect(&self.server).await?;
        let tools = tokio::time::timeout(Duration::from_secs(30), client.list_tools())
            .await
            .map_err(|_| format!("`{}` didn't answer", self.server.name))??;
        Ok(tools.into_iter().map(|t| t.name).collect())
    }

    async fn call(&self, tool: &str, args: Value) -> Result<String, String> {
        let client = self.manager.get_or_connect(&self.server).await?;
        let raw = tokio::time::timeout(Duration::from_secs(60), client.execute_tool(tool, args))
            .await
            .map_err(|_| format!("`{tool}` timed out"))??;
        match serde_json::from_value::<McpCallToolResult>(raw.clone()) {
            Ok(res) if res.is_error => Err(res.format_output()),
            Ok(res) => Ok(res.format_output()),
            Err(_) => Ok(raw
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| raw.to_string())),
        }
    }
}

/// Calls `direct` if the server has it; some Atlassian MCP servers (the
/// v2 remote one in particular) don't expose every Jira operation as its
/// own tool — instead `dispatch` (`executeRead`/`executeWrite`) runs any
/// operation by name, found through `discover`. `generic_op` is that
/// operation's name (it can differ from `direct`'s).
async fn call_or_generic(
    mcp: &dyn Mcp,
    tools: &[String],
    cloud_id: &str,
    direct: &str,
    generic_op: &str,
    dispatch: &str,
    mut inputs: serde_json::Map<String, Value>,
) -> Result<String, String> {
    if let Some(t) = tool(tools, direct) {
        inputs.insert("cloudId".into(), json!(cloud_id));
        return mcp.call(&t, Value::Object(inputs)).await;
    }
    let g = tool(tools, dispatch).ok_or_else(|| {
        format!("the MCP server has no `{direct}` tool (nor `{dispatch}` to fall back to) — is it Atlassian's (https://mcp.atlassian.com/v2/mcp)?")
    })?;
    mcp.call(
        &g,
        json!({"cloudId": cloud_id, "name": generic_op, "inputs": Value::Object(inputs)}),
    )
    .await
}

/// One ticket from a search.
#[derive(Debug, Clone)]
struct Issue {
    key: String,
    summary: String,
    description: String,
    status: String,
    category: String,
    priority: String,
    assignee: Option<String>,
    /// The assignee's account id, when the server sends it.
    assignee_id: Option<String>,
    issue_type: String,
    reporter: String,
    due_date: String,
    labels: Vec<String>,
    created: String,
    updated: String,
}

/// A ticket made a moment ago with this summary, when the create call's
/// answer didn't name it.
async fn find_just_created(
    mcp: &dyn Mcp,
    tools: &[String],
    link: &JiraLink,
    summary: &str,
) -> Option<String> {
    let search = tool(tools, SEARCH)?;
    let words: String = summary
        .chars()
        .map(|c| if c == '"' || c == '\\' { ' ' } else { c })
        .collect();
    let jql = format!(
        "project = \"{}\" AND summary ~ \"{}\" AND created >= -30m ORDER BY created DESC",
        link.project.replace('"', ""),
        words.trim()
    );
    let text = mcp
        .call(
            &search,
            json!({"cloudId": link.cloud_id, "jql": jql, "maxResults": 5, "fields": ["summary"]}),
        )
        .await
        .ok()?;
    json_of(&text)
        .and_then(|v| find_array(&v, "issues").cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(issue_of)
        .find(|i| i.summary.trim().eq_ignore_ascii_case(summary.trim()))
        .map(|i| i.key)
}

/// Why a create call's answer had no ticket key in it.
const NO_KEY: &str = "Jira didn't say which ticket it made";

/// Finds an MCP server with Jira's tools among `servers`.
pub async fn find_server(
    manager: &Arc<McpConnectionManager>,
    servers: &[McpServerConfig],
) -> Result<McpServerConfig, String> {
    let mut tried = Vec::new();
    for s in servers.iter().filter(|s| s.enabled) {
        let mcp = ServerMcp::new(manager.clone(), s.clone());
        match mcp.tools().await {
            Ok(t) if tool(&t, SEARCH).is_some() => return Ok(s.clone()),
            Ok(_) => tried.push(s.name.clone()),
            Err(e) => tried.push(format!("{} ({})", s.name, e.lines().next().unwrap_or(""))),
        }
    }
    Err(if tried.is_empty() {
        "no MCP servers — add Atlassian's: dcode-ai mcp add atlassian --transport http https://mcp.atlassian.com/v1/mcp, then dcode-ai mcp login atlassian".into()
    } else {
        format!(
            "none of the MCP servers has Jira's tools (tried {}). Add Atlassian's: dcode-ai mcp add atlassian --transport http https://mcp.atlassian.com/v1/mcp, then dcode-ai mcp login atlassian",
            tried.join(", ")
        )
    })
}

/// Pages of 100 one sync reads at most.
const MAX_PAGES: usize = 10;

/// Who's signed in, as Jira would name them in an `assignee` field —
/// their email if the server gives one, else their display name.
pub async fn whoami(mcp: &dyn Mcp) -> Result<String, String> {
    let tools = mcp.tools().await?;
    let who = mcp
        .call(&need(&tools, USERINFO)?, json!({}))
        .await
        .map_err(|e| {
            format!(
                "can't tell who's signed in: {}",
                e.lines().next().unwrap_or("")
            )
        })?;
    let v: Value = serde_json::from_str(&who).unwrap_or(Value::Null);
    let data = v.get("data").unwrap_or(&v);
    [
        "email",
        "emailAddress",
        "name",
        "displayName",
        "account_id",
        "accountId",
    ]
    .into_iter()
    .find_map(|k| v.get(k).or_else(|| data.get(k)).and_then(Value::as_str))
    .map(str::to_string)
    .ok_or_else(|| format!("can't tell who's signed in from: {who}"))
}

/// The signed-in person's Atlassian account id.
async fn my_account_id(mcp: &dyn Mcp, tools: &[String]) -> Option<String> {
    let who = mcp.call(&tool(tools, USERINFO)?, json!({})).await.ok()?;
    let v = json_of(&who)?;
    let data = v.get("data").unwrap_or(&v);
    ["accountId", "account_id"]
        .into_iter()
        .find_map(|k| v.get(k).or_else(|| data.get(k)).and_then(Value::as_str))
        .map(str::to_string)
}

/// Links the board to Jira project `project` (on `site`, or the first site
/// the account can see).
pub async fn connect(
    mcp: &dyn Mcp,
    server: &str,
    project: &str,
    site: Option<&str>,
) -> Result<JiraLink, String> {
    let tools = mcp.tools().await?;
    let project = project.trim().to_ascii_uppercase();
    let res = mcp.call(&need(&tools, RESOURCES)?, json!({})).await?;
    let sites: Vec<Value> = match json_of(&res) {
        Some(Value::Array(a)) => a,
        Some(v) => find_array(&v, "resources").cloned().unwrap_or_default(),
        None => Vec::new(),
    };
    let pick = sites
        .iter()
        .find(|s| {
            site.is_some_and(|want| {
                let want = want.to_ascii_lowercase();
                ["url", "name", "cloudId", "id"].iter().any(|k| {
                    s.get(*k)
                        .and_then(Value::as_str)
                        .is_some_and(|v| v.to_ascii_lowercase().contains(&want))
                })
            })
        })
        .or_else(|| site.is_none().then(|| sites.first()).flatten())
        .ok_or_else(|| match site {
            Some(s) => {
                let seen: Vec<&str> = sites
                    .iter()
                    .filter_map(|x| x.get("url").and_then(Value::as_str))
                    .collect();
                if seen.is_empty() {
                    format!("your Atlassian account can't see a site matching `{s}`")
                } else {
                    format!(
                        "your Atlassian account can't see a site matching `{s}` (it sees: {})",
                        seen.join(", ")
                    )
                }
            }
            None => "your Atlassian account can't see any Jira site (signed in? dcode-ai mcp login <name>)".into(),
        })?;
    // Atlassian's remote MCP names it `cloudId`; the REST API names it `id`.
    let cloud_id = pick
        .get("cloudId")
        .or_else(|| pick.get("id"))
        .and_then(Value::as_str)
        .ok_or("the site has no id")?
        .to_string();
    let site_url = pick
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    // The project must answer a search.
    mcp.call(
        &need(&tools, SEARCH)?,
        json!({"cloudId": cloud_id, "jql": format!("project = \"{project}\""), "maxResults": 1, "fields": ["summary"]}),
    )
    .await
    .map_err(|e| format!("can't read project {project}: {}", e.lines().next().unwrap_or("")))?;
    Ok(JiraLink {
        server: server.to_string(),
        cloud_id,
        site: site_url,
        project,
        jql: None,
        assignee: None,
        last_sync: None,
        last_error: None,
        fields: Default::default(),
        sprint: false,
        views: Vec::new(),
        view: None,
        daily_summary: true,
        extra: Default::default(),
    })
}

/// What a sync did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub created: Vec<String>,
    pub moved: usize,
    pub edited: usize,
    pub commented: usize,
    pub logged: usize,
    /// Attachments saved: where each went.
    pub downloaded: Vec<PathBuf>,
    /// What others did to the tickets ("BSI-513: Mamatha commented").
    pub activity: Vec<String>,
    pub pulled: usize,
    /// More tickets matched than one sync reads.
    pub truncated: bool,
    pub errors: Vec<String>,
}

impl Report {
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.created.is_empty() {
            parts.push(format!("created {}", self.created.join(", ")));
        }
        if self.moved > 0 {
            parts.push(format!("moved {}", self.moved));
        }
        if self.edited > 0 {
            parts.push(format!("updated {}", self.edited));
        }
        if self.commented > 0 {
            parts.push(format!("commented {}", self.commented));
        }
        if self.logged > 0 {
            parts.push(format!("logged time on {}", self.logged));
        }
        if !self.downloaded.is_empty() {
            parts.push(format!("saved {} attachment(s)", self.downloaded.len()));
        }
        parts.push(format!(
            "{}{} tickets",
            self.pulled,
            if self.truncated { "+" } else { "" }
        ));
        let mut s = parts.join(" · ");
        if let Some(e) = self.errors.first() {
            s.push_str(&format!(" · ⚠ {e}"));
        }
        s
    }
}

/// Fields every new ticket has anyway (or that we set ourselves).
const FIELDS_WE_SET: &[&str] = &[
    "summary",
    "project",
    "issuetype",
    "reporter",
    "description",
    "assignee",
    "priority",
    "labels",
    "parent",
];

/// One field the project requires on a new ticket that has no default.
#[derive(Debug, Clone)]
struct Required {
    id: String,
    name: String,
    /// `schema.type`: option, date, datetime, timetracking, array, user, …
    kind: String,
    allowed: Vec<Value>,
}

/// What became of a move.
#[derive(Debug, PartialEq)]
enum Moved {
    /// The ticket went through a transition, to this status.
    To(String),
    /// Jira's workflow has no such column; the ticket stays where it is.
    Kept(String),
}

/// One round: send what changed here, then take what's in Jira.
pub async fn sync(mcp: &dyn Mcp, ws: &Path) -> Result<Report, String> {
    let board = Board::load(ws);
    let link = board.jira.clone().ok_or("the board isn't linked to Jira")?;
    let tools = mcp.tools().await?;
    let mut report = Report::default();

    // 1. New Jira cards become tickets.
    let to_create: Vec<(String, String, String, Status)> = board
        .cards
        .iter()
        .filter(|c| c.space == Space::Jira && c.push == Some(Push::Create))
        .map(|c| {
            (
                c.key.clone(),
                c.title.clone(),
                c.description.clone(),
                c.status,
            )
        })
        .collect();
    for (local, title, description, status) in to_create {
        match create(mcp, &tools, &link, &title, &description).await {
            Ok((jira_key, filled)) => {
                let url = browse(&link.site, &jira_key);
                Board::update(ws, |b| {
                    b.rename(&local, &jira_key);
                    if let Some(c) = b.card_mut(&jira_key) {
                        c.remote = Some(Remote {
                            key: jira_key.clone(),
                            url,
                            ..Default::default()
                        });
                        // Made in a column other than To do: move it there.
                        c.push = (status != Status::Todo).then_some(Push::Status);
                        c.note("jira", &format!("ticket {jira_key} created"));
                        if !filled.is_empty() {
                            c.note(
                                "jira",
                                &format!(
                                    "the project requires {}; filled: {} (change with /board jira field <name> = <value>)",
                                    if filled.len() == 1 { "a field" } else { "fields" },
                                    filled.join(" · ")
                                ),
                            );
                        }
                    }
                })
                .map_err(|e| e.to_string())?;
                report.created.push(jira_key);
            }
            Err(e) => {
                // The call went through but no key came back: the ticket may
                // well exist, so trying again every round would make a
                // duplicate each time. Stop and say so.
                if e.starts_with(NO_KEY) {
                    let _ = Board::update(ws, |b| {
                        if let Some(c) = b.card_mut(&local) {
                            c.push = None;
                            c.note(
                                "jira",
                                &format!(
                                    "{} — check Jira for “{title}” before making it again",
                                    first_line(&e)
                                ),
                            );
                        }
                    });
                }
                report
                    .errors
                    .push(format!("couldn't create “{title}”: {}", first_line(&e)))
            }
        }
    }

    // 2. Cards moved here move their tickets.
    let board = Board::load(ws);
    let to_move: Vec<(String, Status)> = board
        .cards
        .iter()
        .filter(|c| c.space == Space::Jira && c.push == Some(Push::Status) && c.remote.is_some())
        .map(|c| (c.shown_key().to_string(), c.status))
        .collect();
    for (key, status) in to_move {
        match move_ticket(mcp, &tools, &link, &key, status).await {
            Ok(moved) => {
                if matches!(moved, Moved::To(_)) {
                    report.moved += 1;
                }
                Board::update(ws, |b| {
                    if let Some(c) = b.card_mut(&key) {
                        c.push = None;
                        match &moved {
                            Moved::To(s) if !s.is_empty() => {
                                if let Some(r) = c.remote.as_mut() {
                                    r.status = s.clone();
                                    // Where it can go next has changed.
                                    r.transitions.clear();
                                    r.detail_of.clear();
                                }
                                c.note("jira", &format!("ticket moved to {s}"));
                            }
                            Moved::To(_) => {}
                            Moved::Kept(why) => c.note("jira", why),
                        }
                    }
                })
                .map_err(|e| e.to_string())?;
            }
            Err(e) => {
                report.errors.push(first_line(&e));
                // Don't try the same impossible move every minute.
                let _ = Board::update(ws, |b| {
                    if let Some(c) = b.card_mut(&key) {
                        c.push = None;
                        c.note(
                            "jira",
                            &format!("couldn't move the ticket: {}", first_line(&e)),
                        );
                    }
                });
            }
        }
    }

    // 2a. Transitions picked by hand.
    let board = Board::load(ws);
    let wanted: Vec<(String, String)> = board
        .cards
        .iter()
        .filter(|c| c.space == Space::Jira)
        .filter_map(|c| {
            let r = c.remote.as_ref()?;
            (!r.want_transition.is_empty()).then(|| (r.key.clone(), r.want_transition.clone()))
        })
        .collect();
    for (key, want) in wanted {
        let done = transition_named(mcp, &tools, &link, &key, &want).await;
        match &done {
            Ok(_) => report.moved += 1,
            Err(e) => report.errors.push(first_line(e)),
        }
        let _ = Board::update(ws, |b| {
            let Some(c) = b.card_mut(&key) else { return };
            let mut column = None;
            if let Some(r) = c.remote.as_mut() {
                // Picked again meanwhile: that one goes next round.
                if r.want_transition == want {
                    r.want_transition.clear();
                }
                if let Ok((lands, cat)) = &done {
                    r.status = lands.clone();
                    r.transitions.clear();
                    r.detail_of.clear();
                    column = Some(column_for(lands, cat));
                }
            }
            match &done {
                Ok((lands, _)) => {
                    if let Some(col) = column {
                        c.status = col;
                    }
                    // The hand-picked move wins over a column move.
                    c.push = None;
                    c.note("jira", &format!("{want}: ticket now {lands}"));
                }
                Err(e) => c.note(
                    "jira",
                    &format!("couldn't move the ticket: {}", first_line(e)),
                ),
            }
        });
    }

    // 2c. Comments written here go to their tickets.
    let board = Board::load(ws);
    let outgoing: Vec<(String, Vec<OutComment>)> = board
        .cards
        .iter()
        .filter(|c| c.space == Space::Jira)
        .filter_map(|c| {
            let r = c.remote.as_ref()?;
            (!r.outbox.is_empty()).then(|| (r.key.clone(), r.outbox.clone()))
        })
        .collect();
    for (key, outbox) in outgoing {
        let mut sent = Vec::new();
        let mut failed = Vec::new();
        for o in outbox {
            match add_comment(mcp, &tools, &link, &key, &o).await {
                Ok(()) => {
                    report.commented += 1;
                    sent.push(o);
                }
                Err(e) => {
                    report
                        .errors
                        .push(format!("couldn't comment on {key}: {}", first_line(&e)));
                    failed.push((o, e));
                }
            }
        }
        let _ = Board::update(ws, |b| {
            let Some(c) = b.card_mut(&key) else { return };
            if let Some(r) = c.remote.as_mut() {
                // Only what went (or was refused): one written meanwhile
                // waits for the next round.
                for o in sent.iter().chain(failed.iter().map(|(o, _)| o)) {
                    if let Some(i) = r.outbox.iter().position(|x| x == o) {
                        r.outbox.remove(i);
                    }
                }
                if !sent.is_empty() {
                    r.detail_of.clear();
                }
            }
            for (o, e) in &failed {
                c.note(
                    "jira",
                    &format!(
                        "couldn't send the comment ({}): {}",
                        first_line(e),
                        o.body.chars().take(500).collect::<String>()
                    ),
                );
            }
        });
    }

    // 2d. Time logged here goes to the tickets' worklogs.
    let board = Board::load(ws);
    let logs: Vec<(String, Vec<dcode_ai_common::board::OutWorklog>)> = board
        .cards
        .iter()
        .filter(|c| c.space == Space::Jira)
        .filter_map(|c| {
            let r = c.remote.as_ref()?;
            (!r.worklogs.is_empty()).then(|| (r.key.clone(), r.worklogs.clone()))
        })
        .collect();
    for (key, list) in logs {
        let mut done = Vec::new();
        for w in list {
            let res = log_work(mcp, &tools, &link, &key, &w).await;
            match &res {
                Ok(()) => report.logged += 1,
                Err(e) => report
                    .errors
                    .push(format!("couldn't log time on {key}: {}", first_line(e))),
            }
            done.push((w, res));
        }
        let _ = Board::update(ws, |b| {
            let Some(c) = b.card_mut(&key) else { return };
            for (w, res) in &done {
                if let Some(r) = c.remote.as_mut()
                    && let Some(i) = r.worklogs.iter().position(|x| x == w)
                {
                    r.worklogs.remove(i);
                    // The time spent shows on the next read.
                    r.detail_of.clear();
                }
                let what = format!("{} {}", w.time, w.comment).trim().to_string();
                match res {
                    Ok(()) => c.note("jira", &format!("logged {what}")),
                    Err(e) => c.note("jira", &format!("couldn't log {what}: {}", first_line(e))),
                }
            }
        });
    }

    // 2e. Attachments asked for are saved under .dcode-ai/attachments/<KEY>/.
    let board = Board::load(ws);
    let wanted: Vec<(
        String,
        Vec<dcode_ai_common::board::RemoteAttachment>,
        Vec<String>,
    )> = board
        .cards
        .iter()
        .filter_map(|c| {
            let r = c.remote.as_ref()?;
            (!r.downloads.is_empty())
                .then(|| (r.key.clone(), r.attachments.clone(), r.downloads.clone()))
        })
        .collect();
    for (key, attachments, ids) in wanted {
        let mut notes = Vec::new();
        for id in &ids {
            let name = attachments
                .iter()
                .find(|a| &a.id == id)
                .map(|a| a.filename.clone())
                .unwrap_or_else(|| format!("attachment-{id}"));
            match download(mcp, &tools, &link, ws, &key, id, &name).await {
                Ok(path) => {
                    notes.push(format!("saved {}", path.display()));
                    report.downloaded.push(path);
                }
                Err(e) => {
                    report
                        .errors
                        .push(format!("couldn't download {name}: {}", first_line(&e)));
                    notes.push(format!("couldn't download {name}: {}", first_line(&e)));
                }
            }
        }
        let _ = Board::update(ws, |b| {
            let Some(c) = b.card_mut(&key) else { return };
            if let Some(r) = c.remote.as_mut() {
                r.downloads.retain(|d| !ids.contains(d));
            }
            for n in &notes {
                c.note("jira", n);
            }
        });
    }

    // 2b. Cards renamed or redescribed here update their tickets.
    let board = Board::load(ws);
    let to_edit: Vec<(String, String, String)> = board
        .cards
        .iter()
        .filter(|c| {
            c.space == Space::Jira
                && c.push_fields
                && c.remote.is_some()
                && c.push != Some(Push::Create)
        })
        .map(|c| {
            (
                c.shown_key().to_string(),
                c.title.clone(),
                c.description.clone(),
            )
        })
        .collect();
    for (key, title, description) in to_edit {
        let done = edit_ticket(mcp, &tools, &link, &key, &title, &description).await;
        if let Err(e) = &done {
            report
                .errors
                .push(format!("couldn't update {key}: {}", first_line(e)));
        } else {
            report.edited += 1;
        }
        let _ = Board::update(ws, |b| {
            if let Some(c) = b.card_mut(&key) {
                // Only what was sent: an edit made meanwhile goes next round.
                if c.title == title && c.description == description {
                    c.push_fields = false;
                }
                if let Err(e) = &done {
                    c.push_fields = false;
                    c.note(
                        "jira",
                        &format!("couldn't update the ticket: {}", first_line(e)),
                    );
                }
            }
        });
    }

    // 3. The project's tickets come in.
    let jql = pull_jql(&link);
    let mut query = json!({
        "cloudId": link.cloud_id,
        "jql": jql,
        "maxResults": 100,
        "fields": ["summary", "status", "priority", "assignee", "description", "issuetype", "created", "updated", "labels", "duedate", "reporter", "parent"],
    });
    // Atlassian's v2 server: its default (compact) view drops the
    // reporter, the description and account ids; "evidence" keeps them.
    if tool(&tools, "executeRead").is_some() {
        query["view"] = json!("evidence");
    }
    // Page through it all (up to a limit): a big filter isn't cut off at
    // the first hundred.
    let search = need(&tools, SEARCH)?;
    let mut raw: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    let mut found_ok = true;
    let mut complete = false;
    for _ in 0..MAX_PAGES {
        let mut q = query.clone();
        if let Some(t) = &token {
            q["nextPageToken"] = json!(t);
        }
        match mcp.call(&search, q).await {
            Ok(text) => {
                let v = json_of(&text).unwrap_or(Value::Null);
                let page = find_array(&v, "issues").cloned().unwrap_or_default();
                let last = find_bool(&v, "isLast");
                let next = find_str(&v, "nextPageToken");
                let empty = page.is_empty();
                raw.extend(page);
                if last == Some(true) || next.is_none() || empty || next == token {
                    complete = true;
                    break;
                }
                token = next;
            }
            Err(e) => {
                report
                    .errors
                    .push(format!("couldn't read Jira: {}", first_line(&e)));
                found_ok = false;
                break;
            }
        }
    }
    report.truncated = found_ok && !complete;
    let mut seen = std::collections::HashSet::new();
    let issues: Vec<Issue> = raw
        .iter()
        .filter_map(issue_of)
        .filter(|i| seen.insert(i.key.clone()))
        // Belt and suspenders: whatever the server's JQL did, never show a
        // ticket that isn't actually this person's when a filter is set —
        // a quoting/account-id mismatch in the query must never widen to
        // "everyone's tickets".
        .filter(|i| matches_assignee_filter(link.assignee.as_deref(), i))
        .collect();
    // Only a pull that read everything may forget tickets it didn't see.
    let found_ok = found_ok && complete;
    report.pulled = issues.len();
    let errors = report.errors.clone();
    // Forget Jira cards from an earlier, wider filter (or a project no
    // longer matched) that this pull — a clean read, not an error — no
    // longer accounts for. Cards with something still waiting to go out
    // stay, so a move or edit in flight is never lost.
    let fetch_ok = found_ok;
    let fetched_keys: std::collections::HashSet<&str> =
        issues.iter().map(|i| i.key.as_str()).collect();
    let mut superseded = false;
    let mut activity: Vec<(String, String)> = Vec::new();
    Board::update(ws, |b| {
        // The link changed while this round was out (`/board jira … me`
        // a moment ago): what came back answers the old filter — drop it,
        // never let it fill the board with everyone's tickets.
        let same = b.jira.as_ref().is_some_and(|l| {
            l.project == link.project
                && l.cloud_id == link.cloud_id
                && l.assignee == link.assignee
                && pull_jql(l) == jql
        });
        if !same {
            superseded = true;
            return;
        }
        if fetch_ok {
            b.cards.retain(|c| {
                !(c.space == Space::Jira
                    && c.unsent_nothing()
                    && c.remote
                        .as_ref()
                        .is_some_and(|r| !fetched_keys.contains(r.key.as_str())))
            });
        }
        for i in &issues {
            let status = column_for(&i.status, &i.category);
            let old = b
                .card(&i.key)
                .and_then(|c| c.remote.clone())
                .unwrap_or_default();
            // Moved in Jira by someone else (the board's own moves update
            // the status they land on right away).
            let moved_elsewhere = !old.status.is_empty()
                && old.status != i.status
                && b.card(&i.key).is_some_and(|c| {
                    c.push.is_none()
                        && c.remote
                            .as_ref()
                            .is_some_and(|r| r.want_transition.is_empty())
                });
            if moved_elsewhere {
                activity.push((i.key.clone(), format!("moved to {}", i.status)));
            }
            let remote = Remote {
                key: i.key.clone(),
                url: browse(&link.site, &i.key),
                status: i.status.clone(),
                issue_type: i.issue_type.clone(),
                reporter: i.reporter.clone(),
                due_date: i.due_date.clone(),
                labels: i.labels.clone(),
                created: i.created.clone(),
                updated: i.updated.clone(),
                // Read with the details below; kept until they're read again.
                parent: old.parent,
                fields: old.fields,
                links: old.links,
                comments: old.comments,
                comment_count: old.comment_count,
                detail_of: old.detail_of,
                transitions: old.transitions,
                want_transition: old.want_transition,
                outbox: old.outbox,
                attachments: old.attachments,
                downloads: old.downloads,
                worklogs: old.worklogs,
                activity: if moved_elsewhere {
                    format!("moved to {}", i.status)
                } else {
                    old.activity
                },
            };
            let jira_unchanged = !old.status.is_empty() && old.status == i.status;
            match b.card_mut(&i.key) {
                Some(c) => {
                    // A move here that hasn't gone yet wins over Jira; so
                    // does a Review (Backlog) card whose workflow has no
                    // such status, while Jira hasn't moved the ticket.
                    let refined = jira_unchanged
                        && matches!(
                            (c.status, status),
                            (Status::Review, Status::InProgress) | (Status::Backlog, Status::Todo)
                        );
                    if c.push.is_none() && !refined {
                        c.status = status;
                    }
                    // An edit here that hasn't gone yet wins too.
                    if !c.push_fields {
                        c.title = i.summary.chars().take(200).collect();
                        if !i.description.is_empty() {
                            c.description = i.description.chars().take(4000).collect();
                        }
                    }
                    c.priority = priority_for(&i.priority);
                    c.assignee = i.assignee.clone();
                    c.space = Space::Jira;
                    c.remote = Some(remote);
                }
                None => b.insert_remote(
                    remote,
                    &i.summary,
                    &i.description,
                    status,
                    priority_for(&i.priority),
                    i.assignee.clone(),
                ),
            }
        }
        if let Some(l) = b.jira.as_mut() {
            l.last_sync = Some(chrono::Utc::now());
            l.last_error = errors.first().cloned();
        }
    })
    .map_err(|e| e.to_string())?;
    if superseded {
        // Not an error: whoever changed the filter syncs with the new one.
        report.pulled = 0;
        return Ok(report);
    }

    // 4. Details (custom fields, links, comments) for the tickets that
    // changed since they were last read — one call or two per changed
    // ticket, nothing for the rest.
    let stale: Vec<(String, String)> = {
        let b = Board::load(ws);
        issues
            .iter()
            .filter(|i| {
                b.card(&i.key)
                    .and_then(|c| c.remote.as_ref())
                    .is_none_or(|r| r.detail_of != stamp(i))
            })
            .map(|i| (i.key.clone(), stamp(i)))
            .take(DETAILS_PER_ROUND)
            .collect()
    };
    if !stale.is_empty() && tool(&tools, GET_ISSUE).is_some() {
        // A few at a time, in order: no burst of calls at the server.
        let mut read: Vec<Option<Details>> = Vec::with_capacity(stale.len());
        for chunk in stale.chunks(4) {
            read.extend(
                futures::future::join_all(
                    chunk.iter().map(|(k, _)| details(mcp, &tools, &link, k)),
                )
                .await,
            );
        }
        // New comments on tickets read before: whose? (Yours don't count.)
        let before = Board::load(ws);
        let new_comments = stale.iter().zip(&read).any(|((key, _), d)| {
            d.as_ref().is_some_and(|d| {
                before
                    .card(key)
                    .and_then(|c| c.remote.as_ref())
                    .is_some_and(|r| !r.detail_of.is_empty() && d.comment_count > r.comment_count)
            })
        });
        let me = if new_comments {
            my_account_id(mcp, &tools).await
        } else {
            None
        };
        let _ = Board::update(ws, |b| {
            for ((key, updated), d) in stale.iter().zip(read) {
                let Some(d) = d else { continue };
                if let Some(r) = b.card_mut(key).and_then(|c| c.remote.as_mut()) {
                    if let Some(note) = comment_activity(r, &d, me.as_deref()) {
                        r.activity = note.clone();
                        activity.push((key.clone(), note));
                    }
                    r.parent = d.parent;
                    r.fields = d.fields;
                    r.links = d.links;
                    r.comments = d.comments;
                    r.comment_count = d.comment_count;
                    r.detail_of = updated.clone();
                    if let Some(t) = d.transitions {
                        r.transitions = t;
                    }
                    if let Some(a) = d.attachments {
                        r.attachments = a;
                    }
                    if r.reporter.is_empty() {
                        r.reporter = d.reporter;
                    }
                }
            }
        });
    }
    report.activity = activity
        .into_iter()
        .map(|(k, a)| format!("{k}: {a}"))
        .collect();
    Ok(report)
}

/// At most this many tickets get their details read in one round (the
/// rest wait for the next).
const DETAILS_PER_ROUND: usize = 15;

/// A ticket's details beyond what a search returns.
#[derive(Debug, Default)]
struct Details {
    parent: String,
    reporter: String,
    fields: Vec<(String, String)>,
    links: Vec<String>,
    comments: Vec<dcode_ai_common::board::RemoteComment>,
    comment_count: u32,
    transitions: Option<Vec<RemoteTransition>>,
    attachments: Option<Vec<dcode_ai_common::board::RemoteAttachment>>,
}

/// One line of `/board jira doctor`.
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    /// `✓` fine, `!` works with a caveat, `✗` broken.
    pub mark: char,
    pub what: String,
    pub detail: String,
}

impl Check {
    fn ok(what: &str, detail: impl Into<String>) -> Self {
        Self {
            mark: '✓',
            what: what.into(),
            detail: detail.into(),
        }
    }
    fn warn(what: &str, detail: impl Into<String>) -> Self {
        Self {
            mark: '!',
            what: what.into(),
            detail: detail.into(),
        }
    }
    fn bad(what: &str, detail: impl Into<String>) -> Self {
        Self {
            mark: '✗',
            what: what.into(),
            detail: detail.into(),
        }
    }
    pub fn line(&self) -> String {
        format!("{} {:<14} {}", self.mark, self.what, self.detail)
    }
}

/// The background syncer: a round every minute, right away when kicked
/// (a card made or moved), and whenever something waits to be sent.
pub struct Syncer {
    kick: Arc<tokio::sync::Notify>,
    status: Arc<Mutex<String>>,
    /// What others did to your tickets, and attachments saved, since the
    /// TUI last looked.
    news: Arc<Mutex<(Vec<String>, Vec<PathBuf>)>>,
    busy: Arc<std::sync::atomic::AtomicBool>,
}

impl Syncer {
    pub fn spawn(mcp: Arc<dyn Mcp>, ws: PathBuf) -> Arc<Self> {
        let me = Arc::new(Self {
            kick: Arc::new(tokio::sync::Notify::new()),
            status: Arc::new(Mutex::new("connecting to Jira…".into())),
            news: Default::default(),
            busy: Default::default(),
        });
        let s = me.clone();
        tokio::spawn(async move {
            let mut last = std::time::Instant::now() - Duration::from_secs(3600);
            let mut last_ok = true;
            loop {
                let kicked = tokio::select! {
                    _ = s.kick.notified() => true,
                    _ = tokio::time::sleep(Duration::from_secs(5)) => false,
                };
                let board = Board::load(&ws);
                if board.jira.is_none() {
                    if Arc::strong_count(&s) == 1 {
                        return;
                    }
                    continue;
                }
                // Something waiting goes right away — unless the last round
                // failed, then it waits for the minute like everything else
                // (no hammering Jira with a create it keeps refusing).
                let due = kicked
                    || (board.pending() > 0 && last_ok)
                    || last.elapsed() >= Duration::from_secs(60);
                if !due {
                    continue;
                }
                last = std::time::Instant::now();
                s.busy.store(true, std::sync::atomic::Ordering::SeqCst);
                let text = match sync(mcp.as_ref(), &ws).await {
                    Ok(r) => {
                        last_ok = r.errors.is_empty();
                        if (!r.activity.is_empty() || !r.downloaded.is_empty())
                            && let Ok(mut n) = s.news.lock()
                        {
                            n.0.extend(r.activity.iter().cloned());
                            n.1.extend(r.downloaded.iter().cloned());
                        }
                        r.summary()
                    }
                    Err(e) => {
                        last_ok = false;
                        format!("⚠ {}", first_line(&e))
                    }
                };
                s.busy.store(false, std::sync::atomic::Ordering::SeqCst);
                if let Ok(mut st) = s.status.lock() {
                    *st = text;
                }
                // The session ended: stop.
                if Arc::strong_count(&s) == 1 {
                    return;
                }
            }
        });
        me
    }

    /// Activity on your tickets and attachments saved since the last call.
    pub fn take_news(&self) -> (Vec<String>, Vec<PathBuf>) {
        self.news
            .lock()
            .map(|mut n| std::mem::take(&mut *n))
            .unwrap_or_default()
    }

    /// Sync now.
    pub fn kick(&self) {
        self.kick.notify_one();
    }

    /// The last round, in words.
    pub fn status(&self) -> String {
        if self.busy.load(std::sync::atomic::Ordering::SeqCst) {
            return "syncing…".into();
        }
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

#[cfg(test)]
pub(crate) mod fake;

#[cfg(test)]
mod tests;
