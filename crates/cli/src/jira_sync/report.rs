use super::*;

/// "Mamatha commented" when the ticket (read before) has a comment that
/// wasn't there, and the newest isn't yours.
pub(super) fn comment_activity(old: &Remote, new: &Details, me: Option<&str>) -> Option<String> {
    if old.detail_of.is_empty() || new.comment_count <= old.comment_count {
        return None;
    }
    let newest = new.comments.first()?;
    let mine = me.is_some_and(|m| !newest.author_id.is_empty() && newest.author_id == m);
    if mine {
        return None;
    }
    let who = newest.author.split_whitespace().next().unwrap_or("someone");
    let added = new.comment_count - old.comment_count;
    Some(if added == 1 {
        format!("{who} commented")
    } else {
        format!("{added} new comments ({who} last)")
    })
}

/// When a ticket last changed — or, from a server that doesn't say, a
/// stamp that reads its details once.
pub(super) fn stamp(i: &Issue) -> String {
    if i.updated.is_empty() {
        "read".into()
    } else {
        i.updated.clone()
    }
}

/// `estimate 1d · left 4h · spent 4h` from Jira's time tracking.
pub(super) fn time_text(t: Option<&Value>) -> Option<String> {
    let t = t?;
    let parts: Vec<String> = [
        ("originalEstimate", "estimate"),
        ("remainingEstimate", "left"),
        ("timeSpent", "spent"),
    ]
    .iter()
    .filter_map(|(k, l)| {
        t.get(*k)
            .and_then(Value::as_str)
            .map(|v| format!("{l} {v}"))
    })
    .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// Custom fields not worth a line: board plumbing and service-desk timers.
pub(super) fn noise_field(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    // `[CHART] Time in Status` and friends: Jira's internal encodings.
    if n.starts_with('[') {
        return true;
    }
    [
        "rank",
        "issue color",
        "epic color",
        "epic status",
        "last public comment",
        "request type",
        "request language",
        "satisfaction",
        "time to ",
        "development",
        "vulnerability",
        "flagged",
    ]
    .iter()
    .any(|x| n.contains(x))
}

/// A field's value in words, when it's simple enough to show.
pub(super) fn field_text(v: &Value) -> Option<String> {
    let s = match v {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => if *b { "yes" } else { "no" }.to_string(),
        Value::Object(m) => {
            if m.contains_key("errorMessage") {
                return None;
            }
            ["value", "name", "displayName", "key"]
                .iter()
                .find_map(|k| m.get(*k).and_then(Value::as_str))
                .map(|s| {
                    match m
                        .get("child")
                        .and_then(|c| c.get("value"))
                        .and_then(Value::as_str)
                    {
                        Some(c) => format!("{s} / {c}"),
                        None => s.to_string(),
                    }
                })?
        }
        Value::Array(a) => {
            let parts: Vec<String> = a.iter().filter_map(field_text).collect();
            parts.join(", ")
        }
        Value::Null => return None,
    };
    (!s.is_empty()).then(|| s.chars().take(200).collect())
}

/// Reads one ticket's details (`getJiraIssue`, evidence view) and its
/// newest comments.
pub(super) async fn details(
    mcp: &dyn Mcp,
    tools: &[String],
    link: &JiraLink,
    key: &str,
) -> Option<Details> {
    let text = mcp
        .call(
            &tool(tools, GET_ISSUE)?,
            json!({"cloudId": link.cloud_id, "issueIdOrKey": key, "view": "evidence"}),
        )
        .await
        .ok()?;
    let v = json_of(&text)?;
    let issue = v.get("data").unwrap_or(&v);
    let f = issue.get("fields")?;
    let name = |x: &Value, k: &str| x.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let mut d = Details {
        reporter: f
            .get("reporter")
            .map(|r| name(r, "displayName"))
            .unwrap_or_default(),
        ..Default::default()
    };
    if let Some(p) = f.get("parent") {
        let summary = p
            .get("fields")
            .map(|pf| name(pf, "summary"))
            .unwrap_or_default();
        d.parent = format!("{} {summary}", name(p, "key")).trim().to_string();
    }
    for (label, k) in [
        ("components", "components"),
        ("fix versions", "fixVersions"),
    ] {
        if let Some(t) = f.get(k).and_then(field_text) {
            d.fields.push((label.to_string(), t));
        }
    }
    if let Some(t) = time_text(f.get("timetracking")) {
        {
            d.fields.push(("time".into(), t));
        }
    }
    if let Some(custom) = f.get("customFields").and_then(Value::as_object) {
        for (n, c) in custom {
            if noise_field(n) {
                continue;
            }
            if let Some(t) = c.get("value").and_then(field_text)
                && !t.contains("_*:*")
            {
                d.fields.push((n.clone(), t));
            }
        }
    }
    for st in f
        .get("subtasks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let sf = st.get("fields").unwrap_or(&Value::Null);
        d.links.push(format!(
            "sub-task {} {} ({})",
            name(st, "key"),
            name(sf, "summary"),
            sf.get("status")
                .map(|x| name(x, "name"))
                .unwrap_or_default()
        ));
    }
    for l in f
        .get("issuelinks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let ty = l.get("type").unwrap_or(&Value::Null);
        let (how, other) = match (l.get("outwardIssue"), l.get("inwardIssue")) {
            (Some(o), _) => (name(ty, "outward"), o),
            (_, Some(i)) => (name(ty, "inward"), i),
            _ => continue,
        };
        let of = other.get("fields").unwrap_or(&Value::Null);
        d.links.push(
            format!(
                "{how} {} {} ({})",
                name(other, "key"),
                name(of, "summary"),
                of.get("status")
                    .map(|x| name(x, "name"))
                    .unwrap_or_default()
            )
            .replace(" ()", ""),
        );
    }
    let total = f
        .get("comment")
        .and_then(|c| c.get("total"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    d.comment_count = total;
    let comment_of = |c: &Value| dcode_ai_common::board::RemoteComment {
        author: c
            .get("author")
            .map(|a| name(a, "displayName"))
            .unwrap_or_default(),
        author_id: c
            .get("author")
            .map(|a| name(a, "accountId"))
            .unwrap_or_default(),
        at: name(c, "created"),
        body: c
            .get("body")
            .map(adf_text)
            .unwrap_or_default()
            .trim()
            .chars()
            .take(2000)
            .collect(),
    };
    if total > 0 {
        // Newest first, the latest five.
        let list = match tool(tools, COMMENTS) {
            Some(t) => mcp
                .call(
                    &t,
                    json!({"cloudId": link.cloud_id, "issueIdOrKey": key, "maxResults": 5, "orderBy": "-created"}),
                )
                .await
                .ok()
                .and_then(|t| json_of(&t))
                .and_then(|v| find_array(&v, "comments").cloned()),
            None => None,
        }
        .or_else(|| f.get("comment").and_then(|c| find_array(c, "comments")).map(|a| {
            let mut a = a.clone();
            a.reverse();
            a
        }))
        .unwrap_or_default();
        d.comments = list.iter().take(5).map(comment_of).collect();
    }
    d.transitions = list_transitions(mcp, tools, link, key, false)
        .await
        .ok()
        .map(|ts| ts.iter().filter_map(remote_transition).collect());
    // Attachments and time tracking aren't in the evidence view.
    if let Some(v) = mcp
        .call(
            &tool(tools, GET_ISSUE)?,
            json!({"cloudId": link.cloud_id, "issueIdOrKey": key, "fields": ["attachment", "timetracking"], "fieldsByKeys": true}),
        )
        .await
        .ok()
        .and_then(|t| json_of(&t))
    {
        let issue = v.get("data").unwrap_or(&v);
        let f = issue.get("fields").unwrap_or(&Value::Null);
        d.attachments = Some(
            f.get("attachment")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|a| {
                    Some(dcode_ai_common::board::RemoteAttachment {
                        id: id_str(a.get("id")?)?,
                        filename: a.get("filename")?.as_str()?.to_string(),
                        size: a.get("size").and_then(Value::as_u64).unwrap_or(0),
                    })
                })
                .collect(),
        );
        if let Some(t) = time_text(f.get("timetracking"))
            && !d.fields.iter().any(|(k, _)| k == "time")
        {
            d.fields.push(("time".into(), t));
        }
    }
    Some(d)
}

/// `/board jira doctor`: a live, read-only check of the board's Jira link —
/// the server's tools, who you are, the site, the project, what the pull
/// finds, and one ticket's workflow. Nothing is written to Jira.
pub async fn doctor(mcp: &dyn Mcp, link: &JiraLink) -> Vec<Check> {
    let mut out = Vec::new();
    let tools = match mcp.tools().await {
        Ok(t) => {
            out.push(Check::ok(
                "MCP server",
                format!("`{}` answers · {} tools", link.server, t.len()),
            ));
            t
        }
        Err(e) => {
            out.push(Check::bad(
                "MCP server",
                format!(
                    "`{}`: {} — sign in again: dcode-ai mcp login {}",
                    link.server,
                    first_line(&e),
                    link.server
                ),
            ));
            return out;
        }
    };
    let generic_read = tool(&tools, "executeRead").is_some();
    let generic_write = tool(&tools, "executeWrite").is_some();
    // What the board uses, and whether the server has it (directly or
    // through executeRead / executeWrite).
    let needs: [(&str, &str, bool); 9] = [
        ("search", SEARCH, false),
        ("read ticket", GET_ISSUE, false),
        ("create", CREATE, true),
        ("edit", EDIT, true),
        ("moves", TRANSITIONS, false),
        ("move", TRANSITION, true),
        ("comment", COMMENT, true),
        ("log time", WORKLOG, true),
        ("attachments", DOWNLOAD, false),
    ];
    let mut missing = Vec::new();
    let mut via_generic = Vec::new();
    for (label, name, write) in needs {
        if tool(&tools, name).is_some() {
            continue;
        }
        let generic = if write { generic_write } else { generic_read };
        if generic {
            via_generic.push(label);
        } else {
            missing.push(label);
        }
    }
    out.push(match (missing.is_empty(), via_generic.is_empty()) {
        (true, true) => Check::ok("tools", "everything the board uses is there"),
        (true, false) => Check::ok(
            "tools",
            format!(
                "everything — {} through executeRead/executeWrite",
                via_generic.join(", ")
            ),
        ),
        _ => Check::bad(
            "tools",
            format!(
                "missing: {} — is it Atlassian's server (https://mcp.atlassian.com/v2/mcp)?",
                missing.join(", ")
            ),
        ),
    });
    match my_account_id(mcp, &tools).await {
        Some(id) => out.push(Check::ok("you", format!("signed in as account {id}"))),
        None => out.push(Check::warn(
            "you",
            "can't tell who's signed in — `me` still works (Jira's currentUser())",
        )),
    }
    match tool(&tools, RESOURCES) {
        Some(t) => match mcp.call(&t, json!({})).await {
            Ok(text) => {
                let found = json_of(&text).is_some_and(|v| {
                    let sites = match &v {
                        Value::Array(a) => a.clone(),
                        v => find_array(v, "resources").cloned().unwrap_or_default(),
                    };
                    sites.iter().any(|s| {
                        s.get("cloudId")
                            .or_else(|| s.get("id"))
                            .and_then(Value::as_str)
                            == Some(link.cloud_id.as_str())
                    })
                });
                out.push(if found {
                    Check::ok("site", format!("{} reachable", link.site))
                } else {
                    Check::bad(
                        "site",
                        format!(
                            "your account can't see {} any more — /board jira {} to relink",
                            link.site, link.project
                        ),
                    )
                });
            }
            Err(e) => out.push(Check::bad("site", first_line(&e))),
        },
        None => out.push(Check::warn("site", "the server can't list sites")),
    }
    let Some(search) = tool(&tools, SEARCH) else {
        return out;
    };
    let mut query = json!({
        "cloudId": link.cloud_id,
        "jql": format!("project = \"{}\"", link.project.replace('"', "")),
        "maxResults": 1,
        "fields": ["summary"],
    });
    match mcp.call(&search, query.clone()).await {
        Ok(_) => out.push(Check::ok("project", format!("{} readable", link.project))),
        Err(e) => {
            out.push(Check::bad(
                "project",
                format!("{}: {}", link.project, first_line(&e)),
            ));
            return out;
        }
    }
    let jql = pull_jql(link);
    query["jql"] = json!(jql);
    query["maxResults"] = json!(100);
    query["fields"] = json!(["summary", "status"]);
    let mut first_key = None;
    match mcp.call(&search, query).await {
        Ok(text) => {
            let v = json_of(&text).unwrap_or(Value::Null);
            let issues = find_array(&v, "issues").cloned().unwrap_or_default();
            first_key = issues
                .iter()
                .find_map(|i| i.get("key").and_then(Value::as_str).map(str::to_string));
            let more = find_bool(&v, "isLast") == Some(false);
            let n = format!("{}{}", issues.len(), if more { "+" } else { "" });
            out.push(if issues.is_empty() {
                Check::warn("pull", format!("finds no tickets: {jql}"))
            } else {
                Check::ok("pull", format!("{n} tickets: {jql}"))
            });
        }
        Err(e) => out.push(Check::bad(
            "pull",
            format!("{} — the JQL: {jql}", first_line(&e)),
        )),
    }
    if let Some(key) = first_key {
        match list_transitions(mcp, &tools, link, &key, true).await {
            Ok(ts) => {
                let needs_fields: Vec<String> = ts
                    .iter()
                    .filter(|t| !transition_fields(t).is_empty())
                    .filter_map(remote_transition)
                    .map(|t| t.label())
                    .collect();
                let detail = format!(
                    "{key} can: {}{}",
                    can_go(&ts),
                    if needs_fields.is_empty() {
                        String::new()
                    } else {
                        format!(" (a resolution is filled for: {})", needs_fields.join(", "))
                    }
                );
                out.push(Check::ok("workflow", detail));
            }
            Err(e) => out.push(Check::bad("workflow", format!("{key}: {}", first_line(&e)))),
        }
    }
    out
}

/// `/board jira today`: your Jira day from the board as last synced — no
/// model, no credits. What others did, what's due, what waits on you.
pub fn today_lines(board: &Board, today: chrono::NaiveDate) -> Vec<String> {
    let cards: Vec<&dcode_ai_common::board::Card> = board
        .in_space(Space::Jira)
        .filter(|c| c.remote.is_some())
        .collect();
    let mut lines = vec![match board.jira.as_ref().and_then(|l| l.last_sync) {
        Some(t) => format!(
            "Jira today · {} tickets · as of {}",
            cards.len(),
            t.with_timezone(&chrono::Local).format("%a %H:%M")
        ),
        None => format!("Jira today · {} tickets · not synced yet", cards.len()),
    }];
    let show = |c: &dcode_ai_common::board::Card, extra: &str| -> String {
        let title: String = c.title.chars().take(60).collect();
        format!("  {:<10} {title}{extra}", c.shown_key())
    };
    let mut section = |name: &str, list: Vec<String>| {
        if list.is_empty() {
            return;
        }
        lines.push(String::new());
        lines.push(format!("{name} ({})", list.len()));
        let n = list.len();
        lines.extend(list.into_iter().take(8));
        if n > 8 {
            lines.push(format!("  … and {} more", n - 8));
        }
    };
    fn remote(c: &dcode_ai_common::board::Card) -> &Remote {
        c.remote.as_ref().expect("filtered")
    }
    section(
        "● New activity",
        cards
            .iter()
            .filter(|c| !remote(c).activity.is_empty())
            .map(|c| show(c, &format!(" — {}", remote(c).activity)))
            .collect(),
    );
    let due = |c: &dcode_ai_common::board::Card| {
        chrono::NaiveDate::parse_from_str(remote(c).due_date.get(..10).unwrap_or(""), "%Y-%m-%d")
            .ok()
    };
    section(
        "⚠ Overdue / due today",
        cards
            .iter()
            .filter(|c| c.status != Status::Done && due(c).is_some_and(|d| d <= today))
            .map(|c| {
                let d = due(c).expect("checked");
                show(
                    c,
                    &if d < today {
                        format!(" — overdue since {d}")
                    } else {
                        " — due today".into()
                    },
                )
            })
            .collect(),
    );
    section(
        "↩ Waiting on you (someone else commented last)",
        cards
            .iter()
            .filter(|c| c.status != Status::Done)
            .filter(|c| {
                let r = remote(c);
                r.comments.first().is_some_and(|cm| {
                    c.assignee
                        .as_deref()
                        .is_some_and(|a| !a.eq_ignore_ascii_case(&cm.author))
                })
            })
            .map(|c| show(c, &format!(" — {}", remote(c).comments[0].author)))
            .collect(),
    );
    section(
        "▶ In progress",
        cards
            .iter()
            .filter(|c| matches!(c.status, Status::InProgress | Status::Review))
            .map(|c| show(c, &format!(" — {}", remote(c).status)))
            .collect(),
    );
    section(
        "⛓ Blocked",
        cards
            .iter()
            .filter(|c| c.status != Status::Done)
            .filter(|c| {
                c.blocked_by
                    .iter()
                    .any(|b| board.card(b).is_some_and(|x| x.status != Status::Done))
            })
            .map(|c| show(c, &format!(" — by {}", c.blocked_by.join(", "))))
            .collect(),
    );
    let since = chrono::Utc::now() - chrono::Duration::hours(24);
    section(
        "✚ New on the board (24h)",
        cards
            .iter()
            .filter(|c| c.created >= since && c.status != Status::Done)
            .map(|c| show(c, ""))
            .collect(),
    );
    let todo = cards.iter().filter(|c| c.status == Status::Todo).count();
    lines.push(String::new());
    lines.push(format!(
        "To do {todo} · in progress {} · done {} · /board opens it · ask the agent for a stand-up note from this",
        cards.iter().filter(|c| matches!(c.status, Status::InProgress | Status::Review)).count(),
        cards.iter().filter(|c| c.status == Status::Done).count()
    ));
    lines
}

pub(super) fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").chars().take(160).collect()
}
