use super::*;

/// The transition that takes a ticket to `to`'s column, or the nearest one
/// (Review → an in-progress status, Backlog → a to-do one) when the
/// workflow has nothing that lands there.
pub(super) fn pick(transitions: &[Value], to: Status) -> Option<&Value> {
    let scored = transitions.iter().filter_map(|t| {
        t.get("id")?;
        if t.get("isAvailable").and_then(Value::as_bool) == Some(false) {
            return None;
        }
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
                Status::Todo if matches!(lname.as_str(), "to do" | "todo" | "open") => 4,
                Status::InProgress if lname.contains("progress") => 4,
                Status::Done if lname == "done" => 4,
                Status::Done if lname == "resolved" => 3,
                _ => 2,
            }
        } else if (to == Status::Review && lands == Status::InProgress)
            || (to == Status::Backlog && lands == Status::Todo)
        {
            1
        } else {
            0
        };
        (score > 0).then_some((score, t))
    });
    // The first of the best: Jira lists the usual way first.
    scored
        .fold(None::<(i32, &Value)>, |best, (s, t)| match best {
            Some((bs, _)) if bs >= s => best,
            _ => Some((s, t)),
        })
        .map(|(_, t)| t)
}

pub(super) fn transition_id(t: &Value) -> Option<String> {
    let id = t.get("id")?;
    id.as_str()
        .map(str::to_string)
        .or_else(|| id.as_u64().map(|n| n.to_string()))
}

/// The transition id that takes a ticket to `to`'s column.
#[cfg(test)]
pub(super) fn pick_transition(transitions: &[Value], to: Status) -> Option<String> {
    pick(transitions, to).and_then(transition_id)
}

/// Values for what the transition's screen requires (from
/// `expand=transitions.fields`): a resolution for a move to done — "Done",
/// "Fixed" or the like — which Jira refuses the move without.
pub(super) fn transition_fields(t: &Value) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    let Some(fields) = t.get("fields").and_then(Value::as_object) else {
        return out;
    };
    for (id, meta) in fields {
        let required = meta.get("required").and_then(Value::as_bool) == Some(true);
        let has_default = meta.get("hasDefaultValue").and_then(Value::as_bool) == Some(true);
        let is_resolution = id == "resolution"
            || meta
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|n| n.eq_ignore_ascii_case("resolution"));
        if !required || has_default || !is_resolution {
            continue;
        }
        let allowed = meta
            .get("allowedValues")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let name_of = |v: &Value| {
            v.get("name")
                .or_else(|| v.get("value"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let chosen = ["done", "fixed", "resolved", "complete", "completed"]
            .iter()
            .find_map(|want| {
                allowed
                    .iter()
                    .find(|v| name_of(v).eq_ignore_ascii_case(want))
            })
            .or_else(|| allowed.first());
        match chosen {
            Some(v) => match v.get("id").and_then(Value::as_str) {
                Some(i) => out.insert(id.clone(), json!({"id": i})),
                None => out.insert(id.clone(), json!({"name": name_of(v)})),
            },
            None => out.insert(id.clone(), json!({"name": "Done"})),
        };
    }
    out
}

/// The moves the ticket's workflow allows from where it is now (with what
/// each one's screen requires, when `with_fields`).
pub(super) async fn list_transitions(
    mcp: &dyn Mcp,
    tools: &[String],
    link: &JiraLink,
    key: &str,
    with_fields: bool,
) -> Result<Vec<Value>, String> {
    let mut inputs = serde_json::Map::from_iter([("issueIdOrKey".to_string(), json!(key))]);
    if with_fields {
        inputs.insert("expand".into(), json!("transitions.fields"));
    }
    let list = call_or_generic(
        mcp,
        tools,
        &link.cloud_id,
        TRANSITIONS,
        "listJiraIssueTransitions",
        "executeRead",
        inputs,
    )
    .await?;
    let transitions = json_of(&list)
        .and_then(|v| {
            find_array(&v, "transitions")
                .cloned()
                .or_else(|| v.as_array().cloned())
        })
        .unwrap_or_default();
    if transitions.is_empty() && reads_as_error(&list) {
        return Err(first_line(&list));
    }
    Ok(transitions)
}

/// A transition as the board keeps it.
pub(super) fn remote_transition(t: &Value) -> Option<RemoteTransition> {
    Some(RemoteTransition {
        id: transition_id(t)?,
        name: t
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        to: t
            .get("to")
            .and_then(|x| x.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

/// "Resolve Issue → Resolved, Close Issue → Closed".
pub(super) fn can_go(transitions: &[Value]) -> String {
    transitions
        .iter()
        .filter_map(remote_transition)
        .map(|t| t.label())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Runs transition `t` (filling what its screen requires); the status it
/// lands in and that status's category.
pub(super) async fn apply_transition(
    mcp: &dyn Mcp,
    tools: &[String],
    link: &JiraLink,
    key: &str,
    t: &Value,
) -> Result<(String, String), String> {
    let id = transition_id(t).ok_or("a transition without an id")?;
    let to = t.get("to").unwrap_or(&Value::Null);
    let lands = to
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let category = to
        .get("statusCategory")
        .and_then(|c| c.get("key"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let mut inputs = serde_json::Map::from_iter([
        ("issueIdOrKey".to_string(), json!(key)),
        ("transitionId".to_string(), json!(id)),
    ]);
    let fields = transition_fields(t);
    if !fields.is_empty() {
        inputs.insert("fields".into(), Value::Object(fields));
    }
    let text = call_or_generic(
        mcp,
        tools,
        &link.cloud_id,
        TRANSITION,
        "transitionJiraIssue",
        "executeWrite",
        inputs,
    )
    .await?;
    if reads_as_error(&text) {
        return Err(first_line(&text));
    }
    Ok((lands, category))
}

pub(super) async fn move_ticket(
    mcp: &dyn Mcp,
    tools: &[String],
    link: &JiraLink,
    key: &str,
    to: Status,
) -> Result<Moved, String> {
    let transitions = list_transitions(mcp, tools, link, key, true).await?;
    let Some(t) = pick(&transitions, to) else {
        let can = can_go(&transitions);
        let can = if can.is_empty() {
            String::new()
        } else {
            format!(" (it can: {can})")
        };
        // Already as near as the workflow goes (a Review card on a
        // workflow with no review status, its ticket in progress): fine.
        if matches!(to, Status::Review | Status::Backlog) {
            return Ok(Moved::Kept(format!(
                "Jira's workflow has no {} status — the ticket stays where it is{can}",
                to.label()
            )));
        }
        return Err(format!(
            "{key} has no way to {} in Jira's workflow{can}",
            to.label()
        ));
    };
    let (lands, _) = apply_transition(mcp, tools, link, key, t).await?;
    Ok(Moved::To(lands))
}

/// Runs the transition picked by hand: `want` is its id, its name
/// ("Resolve Issue") or the status it goes to ("Closed").
pub(super) async fn transition_named(
    mcp: &dyn Mcp,
    tools: &[String],
    link: &JiraLink,
    key: &str,
    want: &str,
) -> Result<(String, String), String> {
    let transitions = list_transitions(mcp, tools, link, key, true).await?;
    let t = transitions
        .iter()
        .find(|t| remote_transition(t).is_some_and(|r| r.matches(want)))
        .ok_or_else(|| {
            format!(
                "{key} has no move “{want}” from where it is now (it can: {})",
                can_go(&transitions)
            )
        })?;
    apply_transition(mcp, tools, link, key, t).await
}

/// Logs time on the ticket.
pub(super) async fn log_work(
    mcp: &dyn Mcp,
    tools: &[String],
    link: &JiraLink,
    key: &str,
    w: &dcode_ai_common::board::OutWorklog,
) -> Result<(), String> {
    let mut inputs = serde_json::Map::from_iter([
        ("issueIdOrKey".to_string(), json!(key)),
        ("timeSpent".to_string(), json!(w.time)),
    ]);
    if !w.comment.trim().is_empty() {
        inputs.insert("comment".into(), json!(w.comment));
    }
    let text = call_or_generic(
        mcp,
        tools,
        &link.cloud_id,
        WORKLOG,
        WORKLOG,
        "executeWrite",
        inputs,
    )
    .await?;
    if reads_as_error(&text) {
        return Err(first_line(&text));
    }
    Ok(())
}

/// Saves a ticket's attachment under `.dcode-ai/attachments/<KEY>/`.
pub(super) async fn download(
    mcp: &dyn Mcp,
    tools: &[String],
    link: &JiraLink,
    ws: &Path,
    key: &str,
    id: &str,
    name: &str,
) -> Result<PathBuf, String> {
    let text = call_or_generic(
        mcp,
        tools,
        &link.cloud_id,
        DOWNLOAD,
        DOWNLOAD,
        "executeRead",
        serde_json::Map::from_iter([("attachmentId".to_string(), json!(id))]),
    )
    .await?;
    let url = json_of(&text)
        .and_then(|v| find_str(&v, "downloadUrl"))
        .ok_or_else(|| format!("no download link in the answer: {}", first_line(&text)))?;
    let dir = ws
        .join(".dcode-ai")
        .join("attachments")
        .join(safe_file_name(key));
    let path = dir.join(safe_file_name(name));
    if url.starts_with("file:") || !url.starts_with("http") {
        return Err(format!("unexpected download link: {url}"));
    }
    let bytes = reqwest::Client::new()
        .get(&url)
        .timeout(Duration::from_secs(300))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| e.to_string())?
        .bytes()
        .await
        .map_err(|e| e.to_string())?;
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| e.to_string())?;
    // Downloads never end up in a commit.
    let ignore = ws.join(".dcode-ai").join("attachments").join(".gitignore");
    if !ignore.exists() {
        let _ = tokio::fs::write(&ignore, "*\n").await;
    }
    tokio::fs::write(&path, &bytes)
        .await
        .map_err(|e| e.to_string())?;
    Ok(path)
}

/// Adds a comment to the ticket.
pub(super) async fn add_comment(
    mcp: &dyn Mcp,
    tools: &[String],
    link: &JiraLink,
    key: &str,
    c: &OutComment,
) -> Result<(), String> {
    let mut inputs = serde_json::Map::from_iter([
        ("issueIdOrKey".to_string(), json!(key)),
        ("commentBody".to_string(), json!(c.body)),
    ]);
    if c.internal {
        inputs.insert("jsmCommentType".into(), json!("internalNote"));
    }
    let text = call_or_generic(
        mcp,
        tools,
        &link.cloud_id,
        COMMENT,
        COMMENT,
        "executeWrite",
        inputs,
    )
    .await?;
    if reads_as_error(&text) {
        return Err(first_line(&text));
    }
    Ok(())
}
