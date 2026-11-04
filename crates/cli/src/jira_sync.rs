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

use dcode_ai_common::board::{Board, JiraLink, Priority, Push, Remote, Space, Status};
use dcode_ai_common::config::McpServerConfig;
use dcode_ai_core::tools::mcp::{McpCallToolResult, McpConnectionManager};
use serde_json::{Value, json};

/// The Atlassian MCP server's tools we use.
const SEARCH: &str = "searchJiraIssuesUsingJql";
const CREATE: &str = "createJiraIssue";
const TRANSITIONS: &str = "getTransitionsForJiraIssue";
const TRANSITION: &str = "transitionJiraIssue";
const RESOURCES: &str = "getAccessibleAtlassianResources";
const ISSUE_TYPES: &str = "getJiraProjectIssueTypesMetadata";

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

/// The server's name for `want` (exact, or ending in it: some servers
/// prefix their tools).
fn tool(tools: &[String], want: &str) -> Option<String> {
    let w = want.to_ascii_lowercase();
    tools
        .iter()
        .find(|t| t.eq_ignore_ascii_case(want))
        .or_else(|| tools.iter().find(|t| t.to_ascii_lowercase().ends_with(&w)))
        .cloned()
}

fn need(tools: &[String], want: &str) -> Result<String, String> {
    tool(tools, want).ok_or_else(|| {
        format!("the MCP server has no `{want}` tool — is it Atlassian's (https://mcp.atlassian.com/v1/mcp)?")
    })
}

/// JSON out of a tool's text (it may have words around it).
fn json_of(text: &str) -> Option<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(text.trim()) {
        return Some(v);
    }
    for (open, close) in [('{', '}'), ('[', ']')] {
        if let (Some(a), Some(b)) = (text.find(open), text.rfind(close))
            && b > a
            && let Ok(v) = serde_json::from_str::<Value>(&text[a..=b])
        {
            return Some(v);
        }
    }
    None
}

/// The first array called `key` anywhere in `v`.
fn find_array<'a>(v: &'a Value, key: &str) -> Option<&'a Vec<Value>> {
    match v {
        Value::Object(m) => m
            .get(key)
            .and_then(Value::as_array)
            .or_else(|| m.values().find_map(|x| find_array(x, key))),
        Value::Array(a) => a.iter().find_map(|x| find_array(x, key)),
        _ => None,
    }
}

/// Jira's rich text (Atlassian Document Format) as plain text.
fn adf_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Object(m) => {
            let mut out = String::new();
            if let Some(t) = m.get("text").and_then(Value::as_str) {
                out.push_str(t);
            }
            if let Some(children) = m.get("content").and_then(Value::as_array) {
                for c in children {
                    out.push_str(&adf_text(c));
                }
            }
            if matches!(
                m.get("type").and_then(Value::as_str),
                Some("paragraph" | "heading" | "listItem" | "codeBlock")
            ) {
                out.push('\n');
            }
            out
        }
        Value::Array(a) => a.iter().map(adf_text).collect(),
        _ => String::new(),
    }
}

/// The board column for a Jira status.
pub fn column_for(status_name: &str, category: &str) -> Status {
    let name = status_name.to_ascii_lowercase();
    match category {
        "done" => Status::Done,
        "indeterminate"
            if name.contains("review") || name.contains("qa") || name.contains("test") =>
        {
            Status::Review
        }
        "indeterminate" => Status::InProgress,
        _ if name.contains("backlog") => Status::Backlog,
        _ if name.contains("done") || name.contains("closed") || name.contains("resolved") => {
            Status::Done
        }
        _ if name.contains("progress") => Status::InProgress,
        _ if name.contains("review") => Status::Review,
        _ => Status::Todo,
    }
}

fn priority_for(name: &str) -> Priority {
    match name.to_ascii_lowercase().as_str() {
        "highest" | "blocker" | "critical" | "urgent" => Priority::Urgent,
        "high" | "major" => Priority::High,
        "low" | "lowest" | "minor" | "trivial" => Priority::Low,
        _ => Priority::Medium,
    }
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
    issue_type: String,
}

fn issue_of(v: &Value) -> Option<Issue> {
    let key = v.get("key")?.as_str()?.to_string();
    let f = v.get("fields").unwrap_or(&Value::Null);
    let s = |path: &[&str]| -> String {
        let mut cur = f;
        for p in path {
            cur = cur.get(*p).unwrap_or(&Value::Null);
        }
        cur.as_str().unwrap_or("").to_string()
    };
    Some(Issue {
        summary: s(&["summary"]),
        description: f
            .get("description")
            .map(adf_text)
            .unwrap_or_default()
            .trim()
            .to_string(),
        status: s(&["status", "name"]),
        category: s(&["status", "statusCategory", "key"]),
        priority: s(&["priority", "name"]),
        assignee: Some(s(&["assignee", "displayName"])).filter(|a| !a.is_empty()),
        issue_type: s(&["issuetype", "name"]),
        key,
    })
}

/// The Jira key in a create result (`{"key": "PROJ-12"}`, or in words).
fn created_key(text: &str) -> Option<String> {
    if let Some(k) =
        json_of(text).and_then(|v| v.get("key").and_then(Value::as_str).map(str::to_string))
    {
        return Some(k);
    }
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .find(|w| {
            w.split_once('-').is_some_and(|(p, n)| {
                !p.is_empty()
                    && p.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                    && p.chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                    && !n.is_empty()
                    && n.chars().all(|c| c.is_ascii_digit())
            })
        })
        .map(str::to_string)
}

fn browse(site: &str, key: &str) -> String {
    if site.is_empty() {
        String::new()
    } else {
        format!("{}/browse/{key}", site.trim_end_matches('/'))
    }
}

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
                ["url", "name", "id"].iter().any(|k| {
                    s.get(*k)
                        .and_then(Value::as_str)
                        .is_some_and(|v| v.to_ascii_lowercase().contains(&want))
                })
            })
        })
        .or_else(|| site.is_none().then(|| sites.first()).flatten())
        .ok_or_else(|| match site {
            Some(s) => format!("your Atlassian account can't see a site matching `{s}`"),
            None => "your Atlassian account can't see any Jira site (signed in? dcode-ai mcp login <name>)".into(),
        })?;
    let cloud_id = pick
        .get("id")
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
        last_sync: None,
        last_error: None,
    })
}

/// What a sync did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub created: Vec<String>,
    pub moved: usize,
    pub pulled: usize,
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
        parts.push(format!("{} tickets", self.pulled));
        let mut s = parts.join(" · ");
        if let Some(e) = self.errors.first() {
            s.push_str(&format!(" · ⚠ {e}"));
        }
        s
    }
}

async fn create(
    mcp: &dyn Mcp,
    tools: &[String],
    link: &JiraLink,
    summary: &str,
    description: &str,
) -> Result<String, String> {
    let name = need(tools, CREATE)?;
    let args = |issue_type: &str| {
        let mut a = json!({
            "cloudId": link.cloud_id,
            "projectKey": link.project,
            "issueTypeName": issue_type,
            "summary": summary,
        });
        if !description.trim().is_empty() {
            a["description"] = json!(description);
        }
        a
    };
    let first = mcp.call(&name, args("Task")).await;
    let text = match first {
        Ok(t) => t,
        Err(e) => {
            // No "Task" in this project: take its first standard type.
            let Some(types_tool) = tool(tools, ISSUE_TYPES) else {
                return Err(e);
            };
            let types = mcp
                .call(
                    &types_tool,
                    json!({"cloudId": link.cloud_id, "projectIdOrKey": link.project}),
                )
                .await?;
            let ty = json_of(&types)
                .and_then(|v| {
                    find_array(&v, "issueTypes")
                        .or_else(|| find_array(&v, "values"))
                        .and_then(|a| {
                            a.iter()
                                .filter(|t| {
                                    !t.get("subtask").and_then(Value::as_bool).unwrap_or(false)
                                })
                                .find_map(|t| {
                                    t.get("name").and_then(Value::as_str).map(str::to_string)
                                })
                        })
                })
                .ok_or(e)?;
            mcp.call(&name, args(&ty)).await?
        }
    };
    created_key(&text).ok_or_else(|| "Jira didn't say which ticket it made".into())
}

/// The transition id that takes a ticket to `to`'s column.
fn pick_transition(transitions: &[Value], to: Status) -> Option<String> {
    let scored = transitions.iter().filter_map(|t| {
        let id = t
            .get("id")?
            .as_str()
            .map(str::to_string)
            .or_else(|| t.get("id")?.as_u64().map(|n| n.to_string()))?;
        let name = t
            .get("to")
            .and_then(|x| x.get("name"))
            .or_else(|| t.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let cat = t
            .get("to")
            .and_then(|x| x.get("statusCategory"))
            .and_then(|x| x.get("key"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let lands = column_for(&name, &cat);
        let lname = name.to_ascii_lowercase();
        let score = if lands == to {
            match to {
                Status::Todo if lname == "to do" || lname == "todo" => 3,
                Status::InProgress if lname.contains("progress") => 3,
                Status::Done if lname == "done" => 3,
                _ => 2,
            }
        } else if to == Status::Review && lands == Status::InProgress {
            1
        } else {
            0
        };
        (score > 0).then_some((score, id))
    });
    scored.max_by_key(|(s, _)| *s).map(|(_, id)| id)
}

async fn move_ticket(
    mcp: &dyn Mcp,
    tools: &[String],
    link: &JiraLink,
    key: &str,
    to: Status,
) -> Result<(), String> {
    let list = mcp
        .call(
            &need(tools, TRANSITIONS)?,
            json!({"cloudId": link.cloud_id, "issueIdOrKey": key}),
        )
        .await?;
    let transitions = json_of(&list)
        .and_then(|v| {
            find_array(&v, "transitions")
                .cloned()
                .or_else(|| v.as_array().cloned())
        })
        .unwrap_or_default();
    let Some(id) = pick_transition(&transitions, to) else {
        return Err(format!(
            "{key} has no way to {} in Jira's workflow",
            to.label()
        ));
    };
    mcp.call(
        &need(tools, TRANSITION)?,
        json!({"cloudId": link.cloud_id, "issueIdOrKey": key, "transition": {"id": id}}),
    )
    .await
    .map(|_| ())
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
            Ok(jira_key) => {
                let url = browse(&link.site, &jira_key);
                Board::update(ws, |b| {
                    b.rename(&local, &jira_key);
                    if let Some(c) = b.card_mut(&jira_key) {
                        c.remote = Some(Remote {
                            key: jira_key.clone(),
                            url,
                            status: String::new(),
                            issue_type: String::new(),
                        });
                        // Made in a column other than To do: move it there.
                        c.push = (status != Status::Todo).then_some(Push::Status);
                        c.note("jira", &format!("ticket {jira_key} created"));
                    }
                })
                .map_err(|e| e.to_string())?;
                report.created.push(jira_key);
            }
            Err(e) => report
                .errors
                .push(format!("couldn't create “{title}”: {}", first_line(&e))),
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
            Ok(()) => {
                report.moved += 1;
                Board::update(ws, |b| {
                    if let Some(c) = b.card_mut(&key) {
                        c.push = None;
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

    // 3. The project's tickets come in.
    let jql = link.jql.clone().unwrap_or_else(|| {
        format!(
            "project = \"{}\" AND (statusCategory != Done OR updated >= -14d) ORDER BY updated DESC",
            link.project
        )
    });
    let found = mcp
        .call(
            &need(&tools, SEARCH)?,
            json!({
                "cloudId": link.cloud_id,
                "jql": jql,
                "maxResults": 100,
                "fields": ["summary", "status", "priority", "assignee", "description", "issuetype", "updated"],
            }),
        )
        .await;
    let issues: Vec<Issue> = match found {
        Ok(text) => json_of(&text)
            .and_then(|v| find_array(&v, "issues").cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(issue_of)
            .collect(),
        Err(e) => {
            report
                .errors
                .push(format!("couldn't read Jira: {}", first_line(&e)));
            Vec::new()
        }
    };
    report.pulled = issues.len();
    let errors = report.errors.clone();
    Board::update(ws, |b| {
        for i in &issues {
            let status = column_for(&i.status, &i.category);
            let remote = Remote {
                key: i.key.clone(),
                url: browse(&link.site, &i.key),
                status: i.status.clone(),
                issue_type: i.issue_type.clone(),
            };
            match b.card_mut(&i.key) {
                Some(c) => {
                    // A move here that hasn't gone yet wins over Jira.
                    if c.push.is_none() {
                        c.status = status;
                    }
                    c.title = i.summary.chars().take(200).collect();
                    if !i.description.is_empty() {
                        c.description = i.description.chars().take(4000).collect();
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
    Ok(report)
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").chars().take(160).collect()
}

/// The background syncer: a round every minute, right away when kicked
/// (a card made or moved), and whenever something waits to be sent.
pub struct Syncer {
    kick: Arc<tokio::sync::Notify>,
    status: Arc<Mutex<String>>,
    busy: Arc<std::sync::atomic::AtomicBool>,
}

impl Syncer {
    pub fn spawn(mcp: Arc<dyn Mcp>, ws: PathBuf) -> Arc<Self> {
        let me = Arc::new(Self {
            kick: Arc::new(tokio::sync::Notify::new()),
            status: Arc::new(Mutex::new("connecting to Jira…".into())),
            busy: Default::default(),
        });
        let s = me.clone();
        tokio::spawn(async move {
            let mut last = std::time::Instant::now() - Duration::from_secs(3600);
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
                let due =
                    kicked || board.pending() > 0 || last.elapsed() >= Duration::from_secs(60);
                if !due {
                    continue;
                }
                last = std::time::Instant::now();
                s.busy.store(true, std::sync::atomic::Ordering::SeqCst);
                let text = match sync(mcp.as_ref(), &ws).await {
                    Ok(r) => r.summary(),
                    Err(e) => format!("⚠ {}", first_line(&e)),
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
pub(crate) mod fake {
    //! A tiny in-memory Jira behind the Atlassian MCP tools' names.
    use super::*;

    #[derive(Default)]
    pub struct FakeJira {
        pub issues: Mutex<Vec<(String, String, String, String)>>, // key, summary, status, category
        pub next: Mutex<u32>,
        pub calls: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl Mcp for FakeJira {
        async fn tools(&self) -> Result<Vec<String>, String> {
            Ok([SEARCH, CREATE, TRANSITIONS, TRANSITION, RESOURCES]
                .iter()
                .map(|s| s.to_string())
                .collect())
        }

        async fn call(&self, tool: &str, args: Value) -> Result<String, String> {
            self.calls.lock().unwrap().push(tool.to_string());
            match tool {
                RESOURCES => Ok(r#"[{"id":"cloud-1","url":"https://acme.atlassian.net","name":"acme"}]"#.into()),
                SEARCH => {
                    let issues: Vec<Value> = self
                        .issues
                        .lock()
                        .unwrap()
                        .iter()
                        .map(|(k, s, st, cat)| {
                            json!({"key": k, "fields": {
                                "summary": s,
                                "status": {"name": st, "statusCategory": {"key": cat}},
                                "priority": {"name": "High"},
                                "assignee": {"displayName": "Arun"},
                                "description": {"type": "doc", "content": [{"type": "paragraph", "content": [{"type": "text", "text": format!("about {s}")}]}]},
                                "issuetype": {"name": "Task"}
                            }})
                        })
                        .collect();
                    Ok(json!({"issues": issues}).to_string())
                }
                CREATE => {
                    let mut n = self.next.lock().unwrap();
                    *n += 1;
                    let key = format!("{}-{}", args["projectKey"].as_str().unwrap(), 100 + *n);
                    self.issues.lock().unwrap().push((
                        key.clone(),
                        args["summary"].as_str().unwrap().into(),
                        "To Do".into(),
                        "new".into(),
                    ));
                    Ok(json!({"id": "10001", "key": key}).to_string())
                }
                TRANSITIONS => Ok(json!({"transitions": [
                    {"id": "11", "name": "To Do", "to": {"name": "To Do", "statusCategory": {"key": "new"}}},
                    {"id": "21", "name": "Start", "to": {"name": "In Progress", "statusCategory": {"key": "indeterminate"}}},
                    {"id": "31", "name": "Review", "to": {"name": "In Review", "statusCategory": {"key": "indeterminate"}}},
                    {"id": "41", "name": "Done", "to": {"name": "Done", "statusCategory": {"key": "done"}}}
                ]})
                .to_string()),
                TRANSITION => {
                    let key = args["issueIdOrKey"].as_str().unwrap().to_string();
                    let id = args["transition"]["id"].as_str().unwrap().to_string();
                    let (st, cat) = match id.as_str() {
                        "21" => ("In Progress", "indeterminate"),
                        "31" => ("In Review", "indeterminate"),
                        "41" => ("Done", "done"),
                        _ => ("To Do", "new"),
                    };
                    for i in self.issues.lock().unwrap().iter_mut() {
                        if i.0 == key {
                            i.2 = st.into();
                            i.3 = cat.into();
                        }
                    }
                    Ok(String::new())
                }
                other => Err(format!("unknown tool {other}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeJira;
    use super::*;
    use dcode_ai_common::board::NewCard;

    #[tokio::test]
    async fn personal_cards_stay_here_and_jira_cards_sync_both_ways() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("shop");
        std::fs::create_dir_all(&ws).unwrap();
        let jira = FakeJira::default();
        jira.issues.lock().unwrap().push((
            "SHOP-7".into(),
            "Fix checkout".into(),
            "In Progress".into(),
            "indeterminate".into(),
        ));
        let link = connect(&jira, "atlassian", "shop", None).await.unwrap();
        assert_eq!(link.cloud_id, "cloud-1");
        assert_eq!(link.project, "SHOP");
        Board::update(&ws, |b| {
            b.jira = Some(link);
            b.add(
                NewCard {
                    title: "Buy milk".into(),
                    ..Default::default()
                },
                "you",
            );
            b.add(
                NewCard {
                    title: "Add Apple Pay".into(),
                    space: Space::Jira,
                    ..Default::default()
                },
                "you",
            );
        })
        .unwrap();

        let r = sync(&jira, &ws).await.unwrap();
        assert_eq!(r.created, vec!["SHOP-101"], "{r:?}");
        assert!(r.errors.is_empty(), "{r:?}");
        let b = Board::load(&ws);
        // The personal card never went to Jira.
        assert_eq!(b.card("SHO-1").unwrap().space, Space::Personal);
        assert!(
            jira.issues
                .lock()
                .unwrap()
                .iter()
                .all(|i| i.1 != "Buy milk")
        );
        // The new card took its ticket's key; the existing ticket came in.
        let made = b.card("SHOP-101").unwrap();
        assert_eq!(made.space, Space::Jira);
        assert_eq!(
            made.remote.as_ref().unwrap().url,
            "https://acme.atlassian.net/browse/SHOP-101"
        );
        let pulled = b.card("SHOP-7").unwrap();
        assert_eq!(pulled.status, Status::InProgress);
        assert_eq!(pulled.priority, Priority::High);
        assert_eq!(pulled.assignee.as_deref(), Some("Arun"));
        assert!(pulled.description.contains("about Fix checkout"));

        // Moving a card here moves its ticket.
        Board::update(&ws, |b| {
            b.card_mut("SHOP-101")
                .unwrap()
                .set_status(Status::Done, "you")
        })
        .unwrap();
        let r = sync(&jira, &ws).await.unwrap();
        assert_eq!(r.moved, 1, "{r:?}");
        let issues = jira.issues.lock().unwrap().clone();
        assert!(issues.iter().any(|i| i.0 == "SHOP-101" && i.2 == "Done"));
        let b = Board::load(&ws);
        assert_eq!(b.card("SHOP-101").unwrap().status, Status::Done);
        assert!(b.card("SHOP-101").unwrap().push.is_none());
        assert!(b.jira.as_ref().unwrap().last_sync.is_some());
    }

    /// The real path: a stdio MCP server (a stand-in for Atlassian's) found
    /// among the configured ones, then linked and synced.
    #[tokio::test]
    async fn syncs_through_a_real_mcp_server() {
        let python = ["python3", "python"].into_iter().find(|p| {
            std::process::Command::new(p)
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success())
        });
        let Some(python) = python else {
            eprintln!("no python: skipped");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("shop");
        std::fs::create_dir_all(&ws).unwrap();
        let script = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/fake_jira_mcp.py"
        );
        let jira: McpServerConfig = serde_json::from_value(
            json!({"name": "atlassian", "command": python, "args": [script]}),
        )
        .unwrap();
        let manager = Arc::new(McpConnectionManager::new(ws.clone(), None));
        let found = find_server(&manager, std::slice::from_ref(&jira))
            .await
            .unwrap();
        assert_eq!(found.name, "atlassian");
        let mcp = ServerMcp::new(manager.clone(), found);
        let link = connect(&mcp, "atlassian", "SHOP", Some("acme"))
            .await
            .unwrap();
        assert_eq!(link.site, "https://acme.atlassian.net");
        Board::update(&ws, |b| {
            b.jira = Some(link);
            b.add(
                dcode_ai_common::board::NewCard {
                    title: "Apple Pay".into(),
                    space: Space::Jira,
                    ..Default::default()
                },
                "you",
            );
        })
        .unwrap();
        let r = sync(&mcp, &ws).await.unwrap();
        assert_eq!(r.created, vec!["SHOP-101"], "{r:?}");
        assert_eq!(r.pulled, 2, "{r:?}");
        Board::update(&ws, |b| {
            b.card_mut("SHOP-7")
                .unwrap()
                .set_status(Status::Done, "you")
        })
        .unwrap();
        let r = sync(&mcp, &ws).await.unwrap();
        assert_eq!(r.moved, 1, "{r:?}");
        assert_eq!(
            Board::load(&ws).card("SHOP-7").unwrap().status,
            Status::Done
        );
        manager.shutdown_all().await;
    }

    #[test]
    fn statuses_and_transitions_map_to_columns() {
        assert_eq!(column_for("In QA", "indeterminate"), Status::Review);
        assert_eq!(column_for("Backlog", "new"), Status::Backlog);
        assert_eq!(column_for("Selected for Development", "new"), Status::Todo);
        assert_eq!(column_for("Closed", ""), Status::Done);
        assert_eq!(
            created_key("Created issue ACME-42 (id 1001)").as_deref(),
            Some("ACME-42")
        );
        let t: Vec<Value> = serde_json::from_str(
            r#"[{"id":"5","to":{"name":"In Progress","statusCategory":{"key":"indeterminate"}}},{"id":"9","to":{"name":"Done","statusCategory":{"key":"done"}}}]"#,
        )
        .unwrap();
        assert_eq!(pick_transition(&t, Status::Done).as_deref(), Some("9"));
        assert_eq!(pick_transition(&t, Status::Review).as_deref(), Some("5"));
        assert_eq!(pick_transition(&t, Status::Backlog), None);
    }
}
