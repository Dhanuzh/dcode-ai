use super::*;

/// The server's name for `want` (exact, or ending in it: some servers
/// prefix their tools).
pub(super) fn tool(tools: &[String], want: &str) -> Option<String> {
    let w = want.to_ascii_lowercase();
    tools
        .iter()
        .find(|t| t.eq_ignore_ascii_case(want))
        .or_else(|| tools.iter().find(|t| t.to_ascii_lowercase().ends_with(&w)))
        .cloned()
}

pub(super) fn need(tools: &[String], want: &str) -> Result<String, String> {
    tool(tools, want).ok_or_else(|| {
        format!("the MCP server has no `{want}` tool — is it Atlassian's (https://mcp.atlassian.com/v1/mcp)?")
    })
}

/// JSON out of a tool's text (it may have words around it).
pub(super) fn json_of(text: &str) -> Option<Value> {
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
pub(super) fn find_array<'a>(v: &'a Value, key: &str) -> Option<&'a Vec<Value>> {
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
pub(super) fn adf_text(v: &Value) -> String {
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

pub(super) fn priority_for(name: &str) -> Priority {
    match name.to_ascii_lowercase().as_str() {
        "highest" | "blocker" | "critical" | "urgent" => Priority::Urgent,
        "high" | "major" => Priority::High,
        "low" | "lowest" | "minor" | "trivial" => Priority::Low,
        _ => Priority::Medium,
    }
}

pub(super) fn issue_of(v: &Value) -> Option<Issue> {
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
        created: s(&["created"]),
        updated: s(&["updated"]),
        labels,
        key,
    })
}

/// The Jira key in a create result (`{"key": "PROJ-12"}`, or in words).
pub(super) fn created_key(text: &str) -> Option<String> {
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

pub(super) fn is_issue_key(w: &str) -> bool {
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
pub(super) fn is_account_id(s: &str) -> bool {
    let s = s.trim();
    !s.contains('@')
        && !s.contains(' ')
        && (s.contains(':') || (s.len() >= 20 && s.chars().all(|c| c.is_ascii_alphanumeric())))
}

pub(super) fn browse(site: &str, key: &str) -> String {
    if site.is_empty() {
        String::new()
    } else {
        format!("{}/browse/{key}", site.trim_end_matches('/'))
    }
}

/// The first boolean called `key` anywhere in `v`.
pub(super) fn find_bool(v: &Value, key: &str) -> Option<bool> {
    match v {
        Value::Object(m) => m
            .get(key)
            .and_then(Value::as_bool)
            .or_else(|| m.values().find_map(|x| find_bool(x, key))),
        _ => None,
    }
}

/// The first string called `key` anywhere in `v` (not inside arrays).
pub(super) fn find_str(v: &Value, key: &str) -> Option<String> {
    match v {
        Value::Object(m) => m
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| m.values().find_map(|x| find_str(x, key))),
        _ => None,
    }
}

/// What a sync pulls: the link's own JQL, or the project's tickets —
/// one person's when filtered — open or touched in the last two weeks,
/// or those in the open sprints.
pub fn pull_jql(link: &JiraLink) -> String {
    if let Some(v) = link.active_view() {
        return v.jql.clone();
    }
    if let Some(j) = link.jql.as_deref().filter(|j| !j.trim().is_empty()) {
        return j.to_string();
    }
    let project = link.project.replace('"', "");
    let mut q = format!("project = \"{project}\"");
    if let Some(a) = link.assignee.as_deref().filter(|a| !a.trim().is_empty()) {
        q.push_str(&format!(" AND assignee = {}", assignee_value(a)));
    }
    if link.sprint {
        q.push_str(" AND sprint in openSprints()");
    } else {
        q.push_str(" AND (statusCategory != Done OR updated >= -14d)");
    }
    q.push_str(" ORDER BY updated DESC");
    q
}

/// `currentUser()` for `me`, else the name quoted for JQL.
pub(super) fn assignee_value(name: &str) -> String {
    if is_me(name) {
        "currentUser()".to_string()
    } else {
        format!(
            "\"{}\"",
            name.trim().replace('\\', "\\\\").replace('"', "\\\"")
        )
    }
}

/// What a sync pulls by default: the project's open and recent tickets.
pub fn project_jql(project: &str) -> String {
    format!(
        "project = \"{project}\" AND (statusCategory != Done OR updated >= -14d) ORDER BY updated DESC"
    )
}

/// `me`: whoever is signed in to the MCP server — Jira's `currentUser()`,
/// so no lookup of who that is can go wrong.
pub fn is_me(filter: &str) -> bool {
    let f = filter.trim();
    f.eq_ignore_ascii_case("me") || f.eq_ignore_ascii_case("currentUser()")
}

/// Only the project's tickets assigned to `name` (`me`, a Jira display
/// name, email or account id), open or touched in the last two weeks.
pub fn assignee_jql(project: &str, name: &str) -> String {
    let who = assignee_value(name);
    format!(
        "project = \"{project}\" AND assignee = {who} AND (statusCategory != Done OR updated >= -14d) ORDER BY updated DESC"
    )
}

/// Whether an issue actually belongs to the configured assignee filter —
/// a client-side check so a server-side JQL quirk (quoting, an account id
/// the server doesn't recognise the same way, a dropped `AND` clause) can
/// never leak everyone's tickets into a board that's meant to show just one
/// person's. `filter` is `None` when the link has no assignee filter at all
/// (everything matches then).
pub(super) fn matches_assignee_filter(filter: Option<&str>, issue: &Issue) -> bool {
    let Some(f) = filter else { return true };
    let f = f.trim();
    if f.is_empty() {
        return true;
    }
    // `currentUser()` in the JQL already picked exactly this person's
    // tickets; an unassigned one can't be theirs.
    if is_me(f) {
        return issue.assignee.is_some();
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

/// An id that may come as a string or a number.
pub(super) fn id_str(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::to_string)
        .or_else(|| v.as_u64().map(|n| n.to_string()))
}

/// Whether a tool's answer is a refusal dressed as a result (some servers
/// don't flag a rejected create as an error).
pub(super) fn reads_as_error(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("repairhint")
        || t.contains("\"error")
        || t.contains("errormessages")
        || t.starts_with("error")
}

/// A file name safe to write: no directories, no `..`.
pub(super) fn safe_file_name(name: &str) -> String {
    let n: String = name
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let n = n.trim().trim_start_matches('.').to_string();
    if n.is_empty() { "attachment".into() } else { n }
}

/// `2h 30m reviewed the PR` → time `2h 30m`, comment `reviewed the PR`.
/// `None` when it doesn't start with a duration (`1w 2d 3h 4m`, `1.5h`).
pub fn parse_worklog(text: &str) -> Option<dcode_ai_common::board::OutWorklog> {
    let is_part = |w: &str| {
        let Some(unit) = w.chars().last() else {
            return false;
        };
        let n = &w[..w.len() - unit.len_utf8()];
        matches!(unit, 'w' | 'd' | 'h' | 'm')
            && !n.is_empty()
            && n.parse::<f64>().is_ok_and(|x| x > 0.0)
    };
    let words: Vec<&str> = text.split_whitespace().collect();
    let n = words
        .iter()
        .take_while(|w| is_part(&w.to_ascii_lowercase()))
        .count();
    if n == 0 {
        return None;
    }
    Some(dcode_ai_common::board::OutWorklog {
        time: words[..n].join(" ").to_ascii_lowercase(),
        comment: words[n..].join(" "),
    })
}
