use super::*;

/// A project's issue types that aren't sub-tasks: `(id, name)`.
pub(super) async fn issue_types(
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

/// What the project insists on for a new `type_id` ticket beyond the
/// summary — Jira refuses the create without them.
pub(super) async fn required_fields(
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
pub(super) fn field_value<'a>(issue: &'a Value, req: &Required) -> Option<&'a Value> {
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
pub(super) fn option_ref(v: &Value) -> Value {
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
pub(super) fn typed_value(req: &Required, v: &str) -> Value {
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
pub(super) fn date_for(req: &Required) -> Value {
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
pub(super) async fn fill_required(
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

/// Makes the ticket; its key, and what was filled in for the project's
/// required fields.
pub(super) async fn create(
    mcp: &dyn Mcp,
    tools: &[String],
    link: &JiraLink,
    summary: &str,
    description: &str,
) -> Result<(String, Vec<String>), String> {
    let name = need(tools, CREATE)?;
    // Filtered to one person: the new ticket is theirs, or the next pull
    // (which only takes their tickets) wouldn't bring it back.
    let my_id = match link.assignee.as_deref() {
        Some(a) if is_me(a) => my_account_id(mcp, tools).await,
        _ => None,
    };
    let owner = my_id.as_deref().or(link
        .assignee
        .as_deref()
        .filter(|a| is_account_id(a))
        .map(str::trim));
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
pub(super) async fn edit_ticket(
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
