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
const ISSUE_TYPE_FIELDS: &str = "getJiraIssueTypeMetaWithFields";
const EDIT: &str = "editJiraIssue";
const USERINFO: &str = "atlassianUserInfo";

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
    /// The assignee's account id, when the server sends it.
    assignee_id: Option<String>,
    issue_type: String,
    reporter: String,
    due_date: String,
    labels: Vec<String>,
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
    let labels = f
        .get("labels")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Some(Issue {
        summary: s(&["summary"]),
        description: f
            .get("description")
            .map(adf_text)
            .unwrap_or_default()
            .trim()
            .to_string(),
        status: s(&["status", "name"]),
        // The v2 server's compact view names the category instead of
        // keying it.
        category: Some(s(&["status", "statusCategory", "key"]))
            .filter(|k| !k.is_empty())
            .unwrap_or_else(|| {
                match s(&["status", "statusCategory", "name"])
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "done" | "complete" | "completed" => "done",
                    "in progress" => "indeterminate",
                    "to do" | "new" => "new",
                    _ => "",
                }
                .to_string()
            }),
        priority: s(&["priority", "name"]),
        assignee: Some(s(&["assignee", "displayName"])).filter(|a| !a.is_empty()),
        assignee_id: Some(s(&["assignee", "accountId"])).filter(|a| !a.is_empty()),
        issue_type: s(&["issuetype", "name"]),
        reporter: s(&["reporter", "displayName"]),
        due_date: s(&["duedate"]),
        labels,
        key,
    })
}

/// The Jira key in a create result (`{"key": "PROJ-12"}`, or in words).
fn created_key(text: &str) -> Option<String> {
    /// The first `"key": "PROJ-42"` anywhere (the v2 server wraps the issue
    /// in `data`/`issue`).
    fn nested(v: &Value) -> Option<String> {
        match v {
            Value::Object(m) => m
                .get("key")
                .and_then(Value::as_str)
                .filter(|k| is_issue_key(k))
                .map(str::to_string)
                .or_else(|| m.values().find_map(nested)),
            Value::Array(a) => a.iter().find_map(nested),
            _ => None,
        }
    }
    if let Some(k) = json_of(text).as_ref().and_then(nested) {
        return Some(k);
    }
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .find(|w| is_issue_key(w))
        .map(str::to_string)
}

fn is_issue_key(w: &str) -> bool {
    w.split_once('-').is_some_and(|(p, n)| {
        !p.is_empty()
            && p.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && p.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            && !n.is_empty()
            && n.chars().all(|c| c.is_ascii_digit())
    })
}

/// An opaque Atlassian account id (`712020:d73c8a38-…`, or a long run of
/// letters and digits) rather than a name or email.
fn is_account_id(s: &str) -> bool {
    let s = s.trim();
    !s.contains('@')
        && !s.contains(' ')
        && (s.contains(':') || (s.len() >= 20 && s.chars().all(|c| c.is_ascii_alphanumeric())))
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

/// What a sync pulls by default: the project's open and recent tickets.
pub fn project_jql(project: &str) -> String {
    format!(
        "project = \"{project}\" AND (statusCategory != Done OR updated >= -14d) ORDER BY updated DESC"
    )
}

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

/// Only the project's tickets assigned to `name` (a Jira display name,
/// email or account id), open or touched in the last two weeks.
pub fn assignee_jql(project: &str, name: &str) -> String {
    let name = name.trim().replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "project = \"{project}\" AND assignee = \"{name}\" AND (statusCategory != Done OR updated >= -14d) ORDER BY updated DESC"
    )
}

/// Whether an issue actually belongs to the configured assignee filter —
/// a client-side check so a server-side JQL quirk (quoting, an account id
/// the server doesn't recognise the same way, a dropped `AND` clause) can
/// never leak everyone's tickets into a board that's meant to show just one
/// person's. `filter` is `None` when the link has no assignee filter at all
/// (everything matches then).
fn matches_assignee_filter(filter: Option<&str>, issue: &Issue) -> bool {
    let Some(f) = filter else { return true };
    let f = f.trim();
    if f.is_empty() {
        return true;
    }
    // An opaque Atlassian account id (e.g. `712020:xxxxxxxx-...` or a long
    // hex/base62 string) isn't something we can compare against the
    // human-readable `displayName` Jira hands back for `assignee`; trust
    // the server's JQL for that shape instead of guessing wrong.
    if is_account_id(f) {
        // Compare ids when the server sent one; otherwise the JQL already
        // asked for exactly this account.
        return match issue.assignee_id.as_deref() {
            Some(id) => id.eq_ignore_ascii_case(f),
            None => issue.assignee.is_some(),
        };
    }
    let Some(a) = issue.assignee.as_deref() else {
        return false;
    };
    let (a, f) = (a.to_ascii_lowercase(), f.to_ascii_lowercase());
    if a == f || a.contains(&f) || f.contains(&a) {
        return true;
    }
    // `f` may be an email (`whoami()` prefers one) while Jira only ever
    // hands back the display name for `assignee` — compare by name parts
    // instead of the raw strings so `jane.doe@acme.com` still matches
    // "Jane Doe".
    let local = f.split('@').next().unwrap_or(&f);
    let tokens: Vec<&str> = local
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| t.len() >= 3)
        .collect();
    // Weak signal (one short token, e.g. a username with no separators):
    // not enough to confidently say it's someone else's ticket, so trust
    // the server rather than risk hiding this person's own work.
    if tokens.len() < 2 {
        return true;
    }
    tokens.iter().all(|t| a.contains(t))
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
        extra: Default::default(),
    })
}

/// What a sync did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub created: Vec<String>,
    pub moved: usize,
    pub edited: usize,
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
        if self.edited > 0 {
            parts.push(format!("updated {}", self.edited));
        }
        parts.push(format!("{} tickets", self.pulled));
        let mut s = parts.join(" · ");
        if let Some(e) = self.errors.first() {
            s.push_str(&format!(" · ⚠ {e}"));
        }
        s
    }
}

/// A project's issue types that aren't sub-tasks: `(id, name)`.
async fn issue_types(
    mcp: &dyn Mcp,
    tools: &[String],
    link: &JiraLink,
) -> Result<Vec<(String, String)>, String> {
    let text = call_or_generic(
        mcp,
        tools,
        &link.cloud_id,
        ISSUE_TYPES,
        "listJiraProjectIssueTypesMetadata",
        "executeRead",
        serde_json::Map::from_iter([("projectIdOrKey".to_string(), json!(link.project))]),
    )
    .await?;
    Ok(json_of(&text)
        .and_then(|v| {
            find_array(&v, "issueTypes")
                .or_else(|| find_array(&v, "values"))
                .cloned()
        })
        .unwrap_or_default()
        .iter()
        .filter(|t| !t.get("subtask").and_then(Value::as_bool).unwrap_or(false))
        .filter_map(|t| {
            let id = id_str(t.get("id")?)?;
            Some((id, t.get("name")?.as_str()?.to_string()))
        })
        .collect())
}

/// An id that may come as a string or a number.
fn id_str(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::to_string)
        .or_else(|| v.as_u64().map(|n| n.to_string()))
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

/// What the project insists on for a new `type_id` ticket beyond the
/// summary — Jira refuses the create without them.
async fn required_fields(
    mcp: &dyn Mcp,
    tools: &[String],
    link: &JiraLink,
    type_id: &str,
) -> Vec<Required> {
    let Ok(text) = call_or_generic(
        mcp,
        tools,
        &link.cloud_id,
        ISSUE_TYPE_FIELDS,
        ISSUE_TYPE_FIELDS,
        "executeRead",
        serde_json::Map::from_iter([
            ("projectIdOrKey".to_string(), json!(link.project)),
            ("issueTypeId".to_string(), json!(type_id)),
            ("requiredFieldsOnly".to_string(), json!(true)),
            ("maxResults".to_string(), json!(100)),
        ]),
    )
    .await
    else {
        return Vec::new();
    };
    json_of(&text)
        .and_then(|v| {
            find_array(&v, "fields")
                .or_else(|| find_array(&v, "values"))
                .cloned()
        })
        .unwrap_or_default()
        .iter()
        .filter(|f| f.get("required").and_then(Value::as_bool).unwrap_or(true))
        .filter(|f| {
            !f.get("hasDefaultValue")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .filter_map(|f| {
            let id = f
                .get("fieldId")
                .or_else(|| f.get("key"))
                .and_then(Value::as_str)?
                .to_string();
            if FIELDS_WE_SET.contains(&id.as_str()) {
                return None;
            }
            Some(Required {
                name: f
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_string(),
                kind: f
                    .get("schema")
                    .and_then(|s| s.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("string")
                    .to_string(),
                allowed: f
                    .get("allowedValues")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
                id,
            })
        })
        .collect()
}

/// A field's value on an issue from a search: the raw `fields.<id>`
/// (Atlassian's v1 server, the REST API) or `fields.customFields.<name>.value`
/// (the v2 server's mapped form).
fn field_value<'a>(issue: &'a Value, req: &Required) -> Option<&'a Value> {
    let f = issue.get("fields")?;
    f.get(&req.id)
        .or_else(|| {
            f.get("customFields")
                .and_then(|c| c.get(&req.name))
                .and_then(|c| c.get("value"))
        })
        .filter(|v| !v.is_null())
}

/// An option as Jira takes it back: by id when there is one.
fn option_ref(v: &Value) -> Value {
    match v.get("id").and_then(id_str) {
        Some(id) => json!({"id": id}),
        None => match v.get("value").or_else(|| v.get("name")) {
            Some(x) => json!({"value": x}),
            None => v.clone(),
        },
    }
}

/// What a user typed in `/board jira field <name> = <value>`, in Jira's
/// shape for the field.
fn typed_value(req: &Required, v: &str) -> Value {
    let v = v.trim();
    match req.kind.as_str() {
        "option" => req
            .allowed
            .iter()
            .find(|a| {
                a.get("value")
                    .or_else(|| a.get("name"))
                    .and_then(Value::as_str)
                    .is_some_and(|x| x.eq_ignore_ascii_case(v))
            })
            .map(option_ref)
            .unwrap_or_else(|| json!({"value": v})),
        "array" => json!(
            v.split(',')
                .map(|x| json!({"value": x.trim()}))
                .collect::<Vec<_>>()
        ),
        "timetracking" => json!({"originalEstimate": v}),
        "number" => v.parse::<f64>().map(|n| json!(n)).unwrap_or(json!(v)),
        "user" => json!({"accountId": v}),
        _ => json!(v),
    }
}

/// A date for a required date field: today for a start, a week out for
/// anything else (an end, a due date).
fn date_for(req: &Required) -> Value {
    let today = chrono::Local::now().date_naive();
    let name = req.name.to_ascii_lowercase();
    let day = if name.contains("start") || name.contains("begin") {
        today
    } else {
        today + chrono::Duration::days(7)
    };
    if req.kind == "datetime" {
        json!(format!("{}T09:00:00.000+0000", day.format("%Y-%m-%d")))
    } else {
        json!(day.format("%Y-%m-%d").to_string())
    }
}

/// Values for the fields the project requires on a new ticket — what the
/// user set with `/board jira field`, else what their latest ticket of the
/// same type had, else a sensible stand-in (dates from today, a 1d
/// estimate, the first choice). Also says, in words, what it filled.
async fn fill_required(
    mcp: &dyn Mcp,
    tools: &[String],
    link: &JiraLink,
    issue_type: &str,
    required: &[Required],
) -> (serde_json::Map<String, Value>, Vec<String>) {
    let mut out = serde_json::Map::new();
    let mut said = Vec::new();
    if required.is_empty() {
        return (out, said);
    }
    // The latest ticket of this type that's yours, as a template.
    let template = match tool(tools, SEARCH) {
        Some(search) => {
            let jql = format!(
                "project = \"{}\" AND issuetype = \"{}\" AND (assignee = currentUser() OR reporter = currentUser()) ORDER BY created DESC",
                link.project.replace('"', ""),
                issue_type.replace('"', "")
            );
            let ids: Vec<&str> = required.iter().map(|r| r.id.as_str()).collect();
            mcp.call(
                &search,
                json!({"cloudId": link.cloud_id, "jql": jql, "maxResults": 1, "fields": ids}),
            )
            .await
            .ok()
            .and_then(|t| json_of(&t))
            .and_then(|v| find_array(&v, "issues").and_then(|a| a.first().cloned()))
        }
        None => None,
    };
    let from = template
        .as_ref()
        .and_then(|t| t.get("key").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();
    for req in required {
        let set = link
            .fields
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(&req.name) || k.eq_ignore_ascii_case(&req.id));
        let theirs = template.as_ref().and_then(|t| field_value(t, req));
        let (value, how) = if let Some((_, v)) = set {
            (typed_value(req, v), "as set".to_string())
        } else {
            match req.kind.as_str() {
                "date" | "datetime" => (date_for(req), String::new()),
                "timetracking" => {
                    let est = theirs
                        .and_then(|t| t.get("originalEstimate"))
                        .and_then(Value::as_str)
                        .unwrap_or("1d")
                        .to_string();
                    (json!({"originalEstimate": est}), String::new())
                }
                "option" => match theirs {
                    Some(v) => (option_ref(v), format!("like {from}")),
                    None => match req.allowed.first() {
                        Some(v) => (option_ref(v), "the first choice".into()),
                        None => continue,
                    },
                },
                "array" => match theirs {
                    Some(Value::Array(a)) => (
                        json!(a.iter().map(option_ref).collect::<Vec<_>>()),
                        format!("like {from}"),
                    ),
                    _ => match req.allowed.first() {
                        Some(v) => (json!([option_ref(v)]), "the first choice".into()),
                        None => continue,
                    },
                },
                "user" => match theirs {
                    Some(v) => (
                        v.get("accountId")
                            .map(|id| json!({"accountId": id}))
                            .unwrap_or(v.clone()),
                        format!("like {from}"),
                    ),
                    None => continue,
                },
                _ => match theirs {
                    Some(v) => (v.clone(), format!("like {from}")),
                    None => continue,
                },
            }
        };
        let shown = match &value {
            Value::String(s) => s.clone(),
            Value::Object(m) => m
                .get("originalEstimate")
                .or_else(|| m.get("value"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    // An option by id: name it.
                    let id = m.get("id")?.as_str()?;
                    req.allowed
                        .iter()
                        .find(|a| a.get("id").and_then(id_str).as_deref() == Some(id))
                        .or_else(|| {
                            theirs.filter(|t| t.get("id").and_then(id_str).as_deref() == Some(id))
                        })
                        .and_then(|a| a.get("value").or_else(|| a.get("name")))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| value.to_string()),
            v => v.to_string(),
        };
        said.push(if how.is_empty() {
            format!("{} {shown}", req.name)
        } else {
            format!("{} {shown} ({how})", req.name)
        });
        out.insert(req.id.clone(), value);
    }
    (out, said)
}

/// Whether a tool's answer is a refusal dressed as a result (some servers
/// don't flag a rejected create as an error).
fn reads_as_error(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("repairhint")
        || t.contains("\"error")
        || t.contains("errormessages")
        || t.starts_with("error")
}

/// Makes the ticket; its key, and what was filled in for the project's
/// required fields.
async fn create(
    mcp: &dyn Mcp,
    tools: &[String],
    link: &JiraLink,
    summary: &str,
    description: &str,
) -> Result<(String, Vec<String>), String> {
    let name = need(tools, CREATE)?;
    // Filtered to one person: the new ticket is theirs, or the next pull
    // (which only takes their tickets) wouldn't bring it back.
    let owner = link
        .assignee
        .as_deref()
        .filter(|a| is_account_id(a))
        .map(str::trim);
    // A Task, or the project's first standard type when it has none.
    let types = issue_types(mcp, tools, link).await.unwrap_or_default();
    let (type_id, issue_type) = types
        .iter()
        .find(|(_, n)| n.eq_ignore_ascii_case("task"))
        .or_else(|| types.first())
        .map(|(i, n)| (Some(i.clone()), n.clone()))
        .unwrap_or((None, "Task".to_string()));
    let required = match &type_id {
        Some(id) => required_fields(mcp, tools, link, id).await,
        None => Vec::new(),
    };
    let (extra, filled) = fill_required(mcp, tools, link, &issue_type, &required).await;
    let args = |owner: Option<&str>| {
        let mut a = json!({
            "cloudId": link.cloud_id,
            "projectKey": link.project,
            "issueType": issue_type,
            "summary": summary,
        });
        if !description.trim().is_empty() {
            a["description"] = json!(description);
        }
        if let Some(o) = owner {
            a["assignee"] = json!(o);
        }
        if !extra.is_empty() {
            a["additional_fields"] = Value::Object(extra.clone());
        }
        a
    };
    let mut answer = mcp.call(&name, args(owner)).await;
    if owner.is_some()
        && answer
            .as_ref()
            .map_or(true, |t| reads_as_error(t) && created_key(t).is_none())
    {
        // Not assignable to them here: make it unassigned.
        answer = mcp.call(&name, args(None)).await;
    }
    let text = answer?;
    if let Some(k) = created_key(&text) {
        return Ok((k, filled));
    }
    if reads_as_error(&text) {
        return Err(first_line(&text));
    }
    if let Some(k) = find_just_created(mcp, tools, link, summary).await {
        return Ok((k, filled));
    }
    Err(format!("{NO_KEY} (it answered: {})", first_line(&text)))
}

/// Sends a card's title and description to its ticket.
async fn edit_ticket(
    mcp: &dyn Mcp,
    tools: &[String],
    link: &JiraLink,
    key: &str,
    title: &str,
    description: &str,
) -> Result<(), String> {
    let text = call_or_generic(
        mcp,
        tools,
        &link.cloud_id,
        EDIT,
        EDIT,
        "executeWrite",
        serde_json::Map::from_iter([
            ("issueIdOrKey".to_string(), json!(key)),
            (
                "fields".to_string(),
                json!({"summary": title, "description": description}),
            ),
        ]),
    )
    .await?;
    if reads_as_error(&text) {
        return Err(first_line(&text));
    }
    Ok(())
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
    let list = call_or_generic(
        mcp,
        tools,
        &link.cloud_id,
        TRANSITIONS,
        "listJiraIssueTransitions",
        "executeRead",
        serde_json::Map::from_iter([("issueIdOrKey".to_string(), json!(key))]),
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
    call_or_generic(
        mcp,
        tools,
        &link.cloud_id,
        TRANSITION,
        "transitionJiraIssue",
        "executeWrite",
        serde_json::Map::from_iter([
            ("issueIdOrKey".to_string(), json!(key)),
            ("transitionId".to_string(), json!(id)),
        ]),
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
            Ok((jira_key, filled)) => {
                let url = browse(&link.site, &jira_key);
                Board::update(ws, |b| {
                    b.rename(&local, &jira_key);
                    if let Some(c) = b.card_mut(&jira_key) {
                        c.remote = Some(Remote {
                            key: jira_key.clone(),
                            url,
                            status: String::new(),
                            issue_type: String::new(),
                            reporter: String::new(),
                            due_date: String::new(),
                            labels: Vec::new(),
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
    let jql = link.jql.clone().unwrap_or_else(|| match &link.assignee {
        Some(name) => assignee_jql(&link.project, name),
        None => project_jql(&link.project),
    });
    let found = mcp
        .call(
            &need(&tools, SEARCH)?,
            json!({
                "cloudId": link.cloud_id,
                "jql": jql,
                "maxResults": 100,
                "fields": ["summary", "status", "priority", "assignee", "description", "issuetype", "updated", "labels", "duedate", "reporter"],
            }),
        )
        .await;
    let found_ok = found.is_ok();
    let issues: Vec<Issue> = match found {
        Ok(text) => json_of(&text)
            .and_then(|v| find_array(&v, "issues").cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(issue_of)
            // Belt and suspenders: whatever the server's JQL did, never show a
            // ticket that isn't actually this person's when a filter is set —
            // a quoting/account-id mismatch in the query must never widen to
            // "everyone's tickets".
            .filter(|i| matches_assignee_filter(link.assignee.as_deref(), i))
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
    // Forget Jira cards from an earlier, wider filter (or a project no
    // longer matched) that this pull — a clean read, not an error — no
    // longer accounts for. Cards with something still waiting to go out
    // stay, so a move or edit in flight is never lost.
    let fetch_ok = found_ok;
    let fetched_keys: std::collections::HashSet<&str> =
        issues.iter().map(|i| i.key.as_str()).collect();
    Board::update(ws, |b| {
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
            let remote = Remote {
                key: i.key.clone(),
                url: browse(&link.site, &i.key),
                status: i.status.clone(),
                issue_type: i.issue_type.clone(),
                reporter: i.reporter.clone(),
                due_date: i.due_date.clone(),
                labels: i.labels.clone(),
            };
            match b.card_mut(&i.key) {
                Some(c) => {
                    // A move here that hasn't gone yet wins over Jira.
                    if c.push.is_none() {
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
                    let id = args["transitionId"].as_str().unwrap().to_string();
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

    /// Answers like Atlassian's remote MCP: sites keyed by `cloudId`, no `name`.
    struct RemoteShape;

    #[async_trait::async_trait]
    impl Mcp for RemoteShape {
        async fn tools(&self) -> Result<Vec<String>, String> {
            Ok(vec![RESOURCES.to_string(), SEARCH.to_string()])
        }
        async fn call(&self, tool: &str, args: Value) -> Result<String, String> {
            match tool {
                RESOURCES => Ok(r#"{"data":{"resources":[{"cloudId":"c-42","url":"https://karmic-sandbox.atlassian.net","products":[{"id":"jira","access":"read-write"}]}]}}"#.into()),
                SEARCH => {
                    assert_eq!(args["cloudId"], "c-42");
                    Ok(r#"{"issues":[]}"#.into())
                }
                _ => Err(format!("unexpected {tool}")),
            }
        }
    }

    #[test]
    fn the_assignee_filter_uses_the_given_name() {
        let q = assignee_jql("BSI", " Dhanush D ");
        assert!(
            q.starts_with("project = \"BSI\" AND assignee = \"Dhanush D\" AND "),
            "{q}"
        );
        assert!(!q.contains("currentUser"));
        let q = assignee_jql("BSI", "a\"b");
        assert!(q.contains(r#"assignee = "a\"b""#), "{q}");
    }

    #[tokio::test]
    async fn whoami_reads_a_nested_account_id() {
        struct Who;
        #[async_trait::async_trait]
        impl Mcp for Who {
            async fn tools(&self) -> Result<Vec<String>, String> {
                Ok(vec![USERINFO.to_string()])
            }
            async fn call(&self, _tool: &str, _args: Value) -> Result<String, String> {
                Ok(json!({"data": {"accountId": "712020:d73c8a38"}}).to_string())
            }
        }
        assert_eq!(whoami(&Who).await.unwrap(), "712020:d73c8a38");
    }

    #[tokio::test]
    async fn connect_reads_cloud_id_from_atlassians_remote_mcp() {
        let link = connect(&RemoteShape, "atlassian", "bsi", None)
            .await
            .unwrap();
        assert_eq!(link.cloud_id, "c-42");
        assert_eq!(link.project, "BSI");
        let link = connect(&RemoteShape, "atlassian", "BSI", Some("karmic"))
            .await
            .unwrap();
        assert_eq!(link.site, "https://karmic-sandbox.atlassian.net");
        let err = connect(&RemoteShape, "atlassian", "BSI", Some("Solution-Internal"))
            .await
            .unwrap_err();
        assert!(
            err.contains("it sees: https://karmic-sandbox.atlassian.net"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn narrowing_the_assignee_filter_drops_stale_tickets_next_sync() {
        // A fake Jira that honours the JQL's `assignee =` clause, like a
        // real server would — unlike FakeJira, which returns everything.
        struct Filtered(Mutex<Vec<(String, String)>>); // key, assignee
        #[async_trait::async_trait]
        impl Mcp for Filtered {
            async fn tools(&self) -> Result<Vec<String>, String> {
                Ok([SEARCH, RESOURCES].iter().map(|s| s.to_string()).collect())
            }
            async fn call(&self, tool: &str, args: Value) -> Result<String, String> {
                match tool {
                    RESOURCES => Ok(
                        r#"[{"id":"cloud-1","url":"https://acme.atlassian.net","name":"acme"}]"#
                            .into(),
                    ),
                    SEARCH => {
                        let jql = args["jql"].as_str().unwrap_or("");
                        let issues: Vec<Value> = self
                            .0
                            .lock()
                            .unwrap()
                            .iter()
                            .filter(|(_, a)| !jql.contains("assignee") || jql.contains(a.as_str()))
                            .map(|(k, a)| {
                                json!({"key": k, "fields": {
                                    "summary": "x", "status": {"name": "To Do", "statusCategory": {"key": "new"}},
                                    "priority": {"name": "High"}, "assignee": {"displayName": a},
                                    "description": null, "issuetype": {"name": "Task"}
                                }})
                            })
                            .collect();
                        Ok(json!({"issues": issues}).to_string())
                    }
                    other => Err(format!("unknown tool {other}")),
                }
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("shop");
        std::fs::create_dir_all(&ws).unwrap();
        let jira = Filtered(Mutex::new(vec![
            ("SHOP-1".into(), "Dhanush".into()),
            ("SHOP-2".into(), "Arun".into()),
            ("SHOP-3".into(), "Priya".into()),
        ]));
        let link = connect(&jira, "atlassian", "shop", None).await.unwrap();
        Board::update(&ws, |b| b.jira = Some(link)).unwrap();

        // Unfiltered: all three come in.
        let r = sync(&jira, &ws).await.unwrap();
        assert_eq!(r.pulled, 3, "{r:?}");
        assert_eq!(Board::load(&ws).in_space(Space::Jira).count(), 3);

        // Narrow the filter to one assignee and resync, as `/board jira
        // shop assignee Dhanush` would.
        Board::update(&ws, |b| {
            b.jira.as_mut().unwrap().assignee = Some("Dhanush".into());
        })
        .unwrap();
        let r = sync(&jira, &ws).await.unwrap();
        assert_eq!(r.pulled, 1, "{r:?}");
        let b = Board::load(&ws);
        let keys: Vec<&str> = b.in_space(Space::Jira).map(|c| c.shown_key()).collect();
        assert_eq!(
            keys,
            vec!["SHOP-1"],
            "stale tickets from the wider filter should be gone: {keys:?}"
        );
    }

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

    #[test]
    fn only_this_persons_tickets_ever_count_as_a_match() {
        let issue = |name: &str| Issue {
            key: "X-1".into(),
            summary: String::new(),
            description: String::new(),
            status: String::new(),
            category: String::new(),
            priority: String::new(),
            assignee: Some(name.to_string()),
            assignee_id: None,
            issue_type: String::new(),
            reporter: String::new(),
            due_date: String::new(),
            labels: Vec::new(),
        };
        // No filter: everything's in.
        assert!(matches_assignee_filter(None, &issue("Anyone")));
        // Exact/display-name match.
        assert!(matches_assignee_filter(
            Some("Jane Doe"),
            &issue("Jane Doe")
        ));
        // `whoami()`'s email still finds the display name.
        assert!(matches_assignee_filter(
            Some("jane.doe@acme.com"),
            &issue("Jane Doe")
        ));
        // A clearly different person is dropped, even if the server's JQL
        // somehow let it through.
        assert!(!matches_assignee_filter(
            Some("jane.doe@acme.com"),
            &issue("John Smith")
        ));
        // Unassigned tickets never count as "mine".
        assert!(!matches_assignee_filter(
            Some("jane.doe@acme.com"),
            &Issue {
                assignee: None,
                ..issue("")
            }
        ));
        // An opaque account id with no id from the server: trust the JQL.
        let me = "712020:aa11bb22-cc33-dd44-ee55-ff6677889900";
        assert!(matches_assignee_filter(Some(me), &issue("Anyone")));
        // With ids from the server, only that account's tickets count.
        let with_id = |id: &str| Issue {
            assignee_id: Some(id.into()),
            ..issue("Someone")
        };
        assert!(matches_assignee_filter(Some(me), &with_id(me)));
        assert!(!matches_assignee_filter(Some(me), &with_id("712020:other")));
        assert!(!matches_assignee_filter(
            Some(me),
            &Issue {
                assignee: None,
                ..issue("")
            }
        ));
    }

    /// Atlassian's v2 remote server, as seen on a real site: no tools of
    /// their own for transitions, issue types or field metadata (they go
    /// through `executeRead`), `data`-wrapped answers, a project whose Task
    /// requires custom fields, and the compact view's named status category.
    struct V2 {
        calls: Mutex<Vec<(String, Value)>>,
        status: Mutex<(String, String)>,
    }

    #[async_trait::async_trait]
    impl Mcp for V2 {
        async fn tools(&self) -> Result<Vec<String>, String> {
            Ok([
                RESOURCES,
                USERINFO,
                SEARCH,
                CREATE,
                EDIT,
                TRANSITION,
                "getJiraIssue",
                "discover",
                "executeRead",
                "executeWrite",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect())
        }
        async fn call(&self, tool: &str, args: Value) -> Result<String, String> {
            self.calls
                .lock()
                .unwrap()
                .push((tool.to_string(), args.clone()));
            let data = |v: Value| Ok(json!({"data": v}).to_string());
            match tool {
                RESOURCES => {
                    Ok(r#"[{"id":"c","url":"https://acme.atlassian.net","name":"acme"}]"#.into())
                }
                "executeRead" => match args["name"].as_str().unwrap() {
                    "listJiraProjectIssueTypesMetadata" => data(json!({"issueTypes": [
                        {"id": "10000", "name": "Epic", "subtask": false},
                        {"id": "10009", "name": "Task", "subtask": false},
                        {"id": "10010", "name": "Sub-task", "subtask": true}
                    ]})),
                    "getJiraIssueTypeMetaWithFields" => {
                        assert_eq!(args["inputs"]["issueTypeId"], "10009");
                        data(json!({"fields": [
                            {"fieldId": "summary", "name": "Summary", "required": true, "schema": {"type": "string"}},
                            {"fieldId": "customfield_10106", "name": "Task Type", "required": true, "schema": {"type": "option"},
                             "allowedValues": [{"id": "10133", "value": "Administrative"}, {"id": "10140", "value": "Build and Development"}]},
                            {"fieldId": "customfield_10122", "name": "Planned Start Date", "required": true, "schema": {"type": "date"}},
                            {"fieldId": "customfield_10123", "name": "Planned End Date", "required": true, "schema": {"type": "date"}},
                            {"fieldId": "timetracking", "name": "Time tracking", "required": true, "schema": {"type": "timetracking"}}
                        ]}))
                    }
                    "listJiraIssueTransitions" => data(json!({"transitions": [
                        {"id": "301", "name": "Start Progress", "to": {"name": "In Progress", "statusCategory": {"key": "indeterminate"}}},
                        {"id": "5", "name": "Resolve Issue", "to": {"name": "Resolved", "statusCategory": {"key": "done"}}}
                    ]})),
                    other => Err(format!("unknown operation {other}")),
                },
                CREATE => {
                    let f = &args["additional_fields"];
                    if f["customfield_10106"].is_null()
                        || f["customfield_10122"].is_null()
                        || f["timetracking"].is_null()
                    {
                        // How the real server refuses: not flagged as an error.
                        return Ok(r#"{"error":"Task Type is required","repairHint":"set customfield_10106"}"#.into());
                    }
                    data(
                        json!({"id": "1", "key": "BSI-600", "self": "https://x/rest/api/3/issue/1"}),
                    )
                }
                TRANSITION => {
                    let to = match args["transitionId"].as_str().unwrap() {
                        "301" => ("In Progress", "In Progress"),
                        _ => ("Resolved", "Done"),
                    };
                    *self.status.lock().unwrap() = (to.0.into(), to.1.into());
                    data(json!({"status": to.0}))
                }
                EDIT => data(json!({"key": args["issueIdOrKey"]})),
                SEARCH => {
                    let jql = args["jql"].as_str().unwrap();
                    if jql.contains("currentUser()") && jql.contains("issuetype") {
                        // The template: your latest Task.
                        return data(
                            json!({"issues": [{"key": "BSI-524", "fields": {"customFields": {
                                "Task Type": {"id": "customfield_10106", "value": {"value": "Build and Development", "id": "10140"}}
                            }}}]}),
                        );
                    }
                    let (st, cat) = self.status.lock().unwrap().clone();
                    let made = self
                        .calls
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|(t, a)| t == CREATE && !a["additional_fields"].is_null());
                    let issues: Vec<Value> = if made {
                        vec![json!({"key": "BSI-600", "fields": {
                            "summary": "From Jira", "status": {"name": st, "statusCategory": {"name": cat}},
                            "assignee": {"displayName": "Dhanush Durai"}, "priority": {"name": "P2"}, "issuetype": {"name": "Task"}
                        }})]
                    } else {
                        Vec::new()
                    };
                    data(json!({"isLast": true, "issues": issues}))
                }
                other => Err(format!("unknown tool {other}")),
            }
        }
    }

    #[tokio::test]
    async fn creates_moves_and_edits_through_atlassians_v2_server() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("bsi");
        std::fs::create_dir_all(&ws).unwrap();
        let jira = V2 {
            calls: Mutex::new(Vec::new()),
            status: Mutex::new(("Open".into(), "To Do".into())),
        };
        let mut link = connect(&jira, "atlassian", "BSI", None).await.unwrap();
        link.assignee = Some("712020:d73c8a38-d2f3-4c0d-82c2-87e216fa1808".into());
        Board::update(&ws, |b| {
            b.jira = Some(link);
            b.add(
                NewCard {
                    title: "NCD and LCD webpart".into(),
                    space: Space::Jira,
                    ..Default::default()
                },
                "you",
            );
        })
        .unwrap();

        // Create: the required fields are filled, the ticket is yours.
        let r = sync(&jira, &ws).await.unwrap();
        assert!(r.errors.is_empty(), "{r:?}");
        assert_eq!(r.created, vec!["BSI-600".to_string()]);
        let create = jira
            .calls
            .lock()
            .unwrap()
            .iter()
            .find(|(t, _)| t == CREATE)
            .map(|(_, a)| a.clone())
            .unwrap();
        assert_eq!(create["issueType"], "Task");
        assert_eq!(
            create["assignee"],
            "712020:d73c8a38-d2f3-4c0d-82c2-87e216fa1808"
        );
        let f = &create["additional_fields"];
        assert_eq!(f["customfield_10106"], json!({"id": "10140"}), "{f}");
        let today = chrono::Local::now()
            .date_naive()
            .format("%Y-%m-%d")
            .to_string();
        assert_eq!(f["customfield_10122"], json!(today));
        assert!(f["customfield_10123"].as_str().unwrap() > today.as_str());
        assert_eq!(f["timetracking"], json!({"originalEstimate": "1d"}));
        let b = Board::load(&ws);
        let c = b.card("BSI-600").unwrap();
        assert!(
            c.notes.iter().any(|n| n
                .text
                .contains("Task Type Build and Development (like BSI-524)")),
            "{:?}",
            c.notes
        );
        // The compact view's "To Do" category lands in To do.
        assert_eq!(c.status, Status::Todo);

        // Move: transitions come through executeRead.
        Board::update(&ws, |b| {
            b.card_mut("BSI-600")
                .unwrap()
                .set_status(Status::InProgress, "you")
        })
        .unwrap();
        let r = sync(&jira, &ws).await.unwrap();
        assert!(r.errors.is_empty(), "{r:?}");
        assert_eq!(r.moved, 1);
        assert_eq!(
            Board::load(&ws).card("BSI-600").unwrap().status,
            Status::InProgress
        );

        // Edit: the new title goes to Jira and isn't undone by the pull.
        Board::update(&ws, |b| {
            let c = b.card_mut("BSI-600").unwrap();
            c.title = "NCD/LCD webpart".into();
            c.edited();
        })
        .unwrap();
        let r = sync(&jira, &ws).await.unwrap();
        assert!(r.errors.is_empty(), "{r:?}");
        assert_eq!(r.edited, 1);
        let edit = jira
            .calls
            .lock()
            .unwrap()
            .iter()
            .find(|(t, _)| t == EDIT)
            .map(|(_, a)| a.clone())
            .unwrap();
        assert_eq!(edit["issueIdOrKey"], "BSI-600");
        assert_eq!(edit["fields"]["summary"], "NCD/LCD webpart");
        assert!(!Board::load(&ws).card("BSI-600").unwrap().push_fields);
    }

    #[tokio::test]
    async fn a_refused_create_is_an_error_not_a_missing_key() {
        // A refusal the server didn't flag as one keeps the card waiting
        // (nothing was made, so trying again is safe) and says why.
        struct Refuse;
        #[async_trait::async_trait]
        impl Mcp for Refuse {
            async fn tools(&self) -> Result<Vec<String>, String> {
                Ok([SEARCH, CREATE, RESOURCES]
                    .iter()
                    .map(|s| s.to_string())
                    .collect())
            }
            async fn call(&self, tool: &str, _args: Value) -> Result<String, String> {
                match tool {
                    RESOURCES => Ok(
                        r#"[{"id":"c","url":"https://acme.atlassian.net","name":"acme"}]"#.into(),
                    ),
                    SEARCH => Ok(r#"{"issues":[]}"#.into()),
                    _ => {
                        Ok(r#"{"error":"Planned Start Date is required","repairHint":"x"}"#.into())
                    }
                }
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("shop");
        std::fs::create_dir_all(&ws).unwrap();
        let link = connect(&Refuse, "atlassian", "shop", None).await.unwrap();
        Board::update(&ws, |b| {
            b.jira = Some(link);
            b.add(
                NewCard {
                    title: "x".into(),
                    space: Space::Jira,
                    ..Default::default()
                },
                "you",
            );
        })
        .unwrap();
        let r = sync(&Refuse, &ws).await.unwrap();
        assert!(
            r.errors[0].contains("Planned Start Date is required"),
            "{r:?}"
        );
        assert_eq!(Board::load(&ws).pending(), 1);
    }

    #[test]
    fn the_created_key_is_found_when_nested() {
        assert_eq!(
            created_key(r#"{"data":{"id":"10001","key":"BSI-534","self":"https://x/rest/api/3/issue/10001"}}"#)
                .as_deref(),
            Some("BSI-534")
        );
        assert_eq!(created_key(r#"{"key":"BSI-9"}"#).as_deref(), Some("BSI-9"));
        assert_eq!(
            created_key("Created issue BSI-77.").as_deref(),
            Some("BSI-77")
        );
        assert_eq!(created_key(r#"{"ok":true}"#), None);
    }

    #[tokio::test]
    async fn a_create_with_no_key_in_the_answer_is_not_retried() {
        // Jira makes the ticket but answers without its key: the card must
        // stop asking, or every round would make another ticket.
        struct Mute(Mutex<u32>);
        #[async_trait::async_trait]
        impl Mcp for Mute {
            async fn tools(&self) -> Result<Vec<String>, String> {
                Ok([SEARCH, CREATE, RESOURCES]
                    .iter()
                    .map(|s| s.to_string())
                    .collect())
            }
            async fn call(&self, tool: &str, _args: Value) -> Result<String, String> {
                match tool {
                    RESOURCES => Ok(
                        r#"[{"id":"c","url":"https://acme.atlassian.net","name":"acme"}]"#.into(),
                    ),
                    SEARCH => Ok(r#"{"issues":[]}"#.into()),
                    CREATE => {
                        *self.0.lock().unwrap() += 1;
                        Ok("done".into())
                    }
                    other => Err(format!("unknown tool {other}")),
                }
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("shop");
        std::fs::create_dir_all(&ws).unwrap();
        let jira = Mute(Mutex::new(0));
        let link = connect(&jira, "atlassian", "shop", None).await.unwrap();
        Board::update(&ws, |b| {
            b.jira = Some(link);
            b.add(
                NewCard {
                    title: "Webpart".into(),
                    space: Space::Jira,
                    ..Default::default()
                },
                "you",
            );
        })
        .unwrap();
        let r = sync(&jira, &ws).await.unwrap();
        assert_eq!(r.errors.len(), 1, "{r:?}");
        assert_eq!(Board::load(&ws).pending(), 0);
        sync(&jira, &ws).await.unwrap();
        assert_eq!(*jira.0.lock().unwrap(), 1, "created twice");
    }
}
