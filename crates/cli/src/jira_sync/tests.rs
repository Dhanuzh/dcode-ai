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
                    r#"[{"id":"cloud-1","url":"https://acme.atlassian.net","name":"acme"}]"#.into(),
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
    let jira: McpServerConfig =
        serde_json::from_value(json!({"name": "atlassian", "command": python, "args": [script]}))
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
        created: String::new(),
        updated: String::new(),
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
                    return Ok(
                        r#"{"error":"Task Type is required","repairHint":"set customfield_10106"}"#
                            .into(),
                    );
                }
                data(json!({"id": "1", "key": "BSI-600", "self": "https://x/rest/api/3/issue/1"}))
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
                RESOURCES => {
                    Ok(r#"[{"id":"c","url":"https://acme.atlassian.net","name":"acme"}]"#.into())
                }
                SEARCH => Ok(r#"{"issues":[]}"#.into()),
                _ => Ok(r#"{"error":"Planned Start Date is required","repairHint":"x"}"#.into()),
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
        created_key(
            r#"{"data":{"id":"10001","key":"BSI-534","self":"https://x/rest/api/3/issue/10001"}}"#
        )
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
                RESOURCES => {
                    Ok(r#"[{"id":"c","url":"https://acme.atlassian.net","name":"acme"}]"#.into())
                }
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

/// Atlassian's v2 server over a BSI-like workflow: Open → In Progress
/// → Resolved/Closed (both needing a resolution), no review status.
struct Workflow {
    calls: Mutex<Vec<(String, Value)>>,
    /// key, assignee, status, category
    issues: Mutex<Vec<(String, String, String, String)>>,
    /// Changes the board's filter while a search is out.
    relink_during_search: Option<PathBuf>,
    /// Someone else has commented on the tickets since.
    new_comment: Mutex<bool>,
    /// Where attachment downloads point.
    download_url: Mutex<String>,
}

impl Workflow {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            issues: Mutex::new(vec![
                (
                    "BSI-1".into(),
                    "Dhanush Durai".into(),
                    "In Progress".into(),
                    "In Progress".into(),
                ),
                (
                    "BSI-2".into(),
                    "Boopathi S".into(),
                    "Open".into(),
                    "To Do".into(),
                ),
                (
                    "BSI-3".into(),
                    "Dinoj D".into(),
                    "In Progress".into(),
                    "In Progress".into(),
                ),
            ]),
            relink_during_search: None,
            new_comment: Mutex::new(false),
            download_url: Mutex::new(String::new()),
        }
    }
    /// `getJiraIssue` reads of a ticket's details (not its attachments).
    fn evidence_reads(&self) -> usize {
        self.called(GET_ISSUE)
            .iter()
            .filter(|a| a["view"] == "evidence")
            .count()
    }
    fn called(&self, tool: &str) -> Vec<Value> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(t, _)| t == tool)
            .map(|(_, a)| a.clone())
            .collect()
    }
}

#[async_trait::async_trait]
impl Mcp for Workflow {
    async fn tools(&self) -> Result<Vec<String>, String> {
        Ok([
            RESOURCES,
            USERINFO,
            SEARCH,
            CREATE,
            EDIT,
            TRANSITION,
            GET_ISSUE,
            COMMENTS,
            COMMENT,
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
            RESOURCES => Ok(r#"[{"id":"c","url":"https://acme.atlassian.net"}]"#.into()),
            USERINFO => data(json!({"accountId": "712020:me"})),
            COMMENT => {
                if args["commentBody"]
                    .as_str()
                    .unwrap_or("")
                    .contains("refuse")
                {
                    return Ok(r#"{"error":true,"message":"You do not have permission"}"#.into());
                }
                data(json!({"commentId": "777"}))
            }
            SEARCH => {
                if let Some(ws) = &self.relink_during_search {
                    Board::update(ws, |b| {
                        b.jira.as_mut().unwrap().assignee = Some("me".into())
                    })
                    .unwrap();
                }
                let jql = args["jql"].as_str().unwrap();
                let mine = jql.contains("assignee = currentUser()");
                let issues: Vec<Value> = self
                        .issues
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|(_, a, _, _)| !mine || a == "Dhanush Durai")
                        .map(|(k, a, st, cat)| json!({"key": k, "fields": {
                            "summary": format!("{k} work"), "status": {"name": st, "statusCategory": {"name": cat}},
                            "assignee": {"displayName": a}, "priority": {"name": "P2"},
                            "updated": if *self.new_comment.lock().unwrap() { "2026-11-05T10:00:00.000-0500" } else { "2026-11-04T10:01:49.471-0500" }
                        }}))
                        .collect();
                data(json!({"isLast": true, "issues": issues}))
            }
            GET_ISSUE
                if args["fields"]
                    .as_array()
                    .is_some_and(|f| f.contains(&json!("attachment"))) =>
            {
                data(json!({"key": args["issueIdOrKey"], "fields": {
                    "attachment": [{"id": "1058600", "filename": "spec.xlsx", "size": 2048}],
                    "timetracking": {"timeSpent": "4h"}
                }}))
            }
            GET_ISSUE => data(json!({"key": args["issueIdOrKey"], "fields": {
                "reporter": {"displayName": "Sreelekshmi Latha"},
                "parent": {"key": "BSI-476", "fields": {"summary": "Release Management"}},
                "comment": {"total": if *self.new_comment.lock().unwrap() { 3 } else { 2 }},
                "issuelinks": [{"type": {"outward": "blocks", "inward": "is blocked by"},
                                "outwardIssue": {"key": "BSI-9", "fields": {"summary": "Go live", "status": {"name": "Open"}}}}],
                "customFields": {
                    "Task Type": {"id": "customfield_10106", "value": {"value": "Build and Development", "id": "10140"}},
                    "Planned End Date": {"id": "customfield_10123", "value": "2026-10-22"},
                    "Rank": {"id": "customfield_10019", "value": "0|i4alp7:"},
                    "Time to resolution": {"id": "customfield_10045", "value": {"errorMessage": "nope"}}
                }
            }})),
            COMMENTS => {
                let mut list = vec![
                    json!({"author": {"displayName": "Mamatha Suresh"}, "created": "2026-11-04T09:00:00.000-0500", "body": "Any update?"}),
                    json!({"author": {"displayName": "Dhanush Durai", "accountId": "712020:me"}, "created": "2026-11-03T09:00:00.000-0500", "body": "On it"}),
                ];
                if *self.new_comment.lock().unwrap() {
                    list.insert(0, json!({"author": {"displayName": "Supriya Prabhu", "accountId": "712020:other"}, "created": "2026-11-05T09:00:00.000-0500", "body": "Please share the ETA"}));
                }
                data(json!({"total": list.len(), "comments": list}))
            }
            "executeWrite" => {
                assert_eq!(args["name"], WORKLOG);
                data(json!({"worklogId": "9001", "timeSpent": args["inputs"]["timeSpent"]}))
            }
            "executeRead" if args["name"] == DOWNLOAD => data(json!({
                "attachmentId": args["inputs"]["attachmentId"],
                "downloadUrl": *self.download_url.lock().unwrap(),
            })),
            "executeRead" => {
                assert_eq!(args["name"], "listJiraIssueTransitions");
                let fields = args["inputs"]["expand"] == "transitions.fields";
                let res = json!({"required": true, "allowedValues": [
                    {"id": "10001", "name": "Won't Fix"}, {"id": "10000", "name": "Fixed"}
                ]});
                let screen = if fields {
                    json!({"resolution": res})
                } else {
                    json!({})
                };
                data(json!({"transitions": [
                    {"id": "301", "name": "Stop Progress", "to": {"name": "Open", "statusCategory": {"key": "new"}}},
                    {"id": "2", "name": "Close Issue", "to": {"name": "Closed", "statusCategory": {"key": "done"}}, "fields": screen},
                    {"id": "5", "name": "Resolve Issue", "to": {"name": "Resolved", "statusCategory": {"key": "done"}}, "fields": screen}
                ]}))
            }
            TRANSITION => {
                let key = args["issueIdOrKey"].as_str().unwrap();
                let id = args["transitionId"].as_str().unwrap();
                if (id == "2" || id == "5") && args["fields"]["resolution"].is_null() {
                    return Ok(r#"{"error":true,"message":"Resolution is required."}"#.into());
                }
                let to = match id {
                    "301" => ("Open", "To Do"),
                    "2" => ("Closed", "Done"),
                    _ => ("Resolved", "Done"),
                };
                for i in self.issues.lock().unwrap().iter_mut() {
                    if i.0 == key {
                        i.2 = to.0.into();
                        i.3 = to.1.into();
                    }
                }
                data(json!({"status": to.0}))
            }
            other => Err(format!("unknown tool {other}")),
        }
    }
}

fn linked(ws: &Path, assignee: Option<&str>) {
    Board::update(ws, |b| {
        b.jira = Some(JiraLink {
            server: "atlassian".into(),
            cloud_id: "c".into(),
            site: "https://acme.atlassian.net".into(),
            project: "BSI".into(),
            jql: None,
            assignee: assignee.map(str::to_string),
            last_sync: None,
            last_error: None,
            fields: Default::default(),
            sprint: false,
            views: Vec::new(),
            view: None,
            daily_summary: true,
            extra: Default::default(),
        })
    })
    .unwrap();
}

#[tokio::test]
async fn me_pulls_only_my_tickets_with_their_details() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().to_path_buf();
    let jira = Workflow::new();
    linked(&ws, Some("me"));
    let r = sync(&jira, &ws).await.unwrap();
    assert!(r.errors.is_empty(), "{r:?}");
    assert_eq!(r.pulled, 1);
    let q = &jira.called(SEARCH)[0];
    assert!(
        q["jql"]
            .as_str()
            .unwrap()
            .contains("assignee = currentUser()"),
        "{q}"
    );
    assert_eq!(q["view"], "evidence");
    let b = Board::load(&ws);
    assert_eq!(b.in_space(Space::Jira).count(), 1);
    let rm = b.card("BSI-1").unwrap().remote.clone().unwrap();
    assert_eq!(rm.parent, "BSI-476 Release Management");
    assert_eq!(rm.reporter, "Sreelekshmi Latha");
    assert!(
        rm.fields
            .contains(&("Task Type".into(), "Build and Development".into())),
        "{:?}",
        rm.fields
    );
    assert!(
        rm.fields
            .contains(&("Planned End Date".into(), "2026-10-22".into()))
    );
    assert!(
        !rm.fields
            .iter()
            .any(|(k, _)| k == "Rank" || k.starts_with("Time to")),
        "{:?}",
        rm.fields
    );
    assert_eq!(rm.links, vec!["blocks BSI-9 Go live (Open)".to_string()]);
    assert_eq!(rm.comment_count, 2);
    assert_eq!(rm.comments[0].author, "Mamatha Suresh");
    assert_eq!(rm.comments[0].body, "Any update?");
    let shown = b.render_card(b.card("BSI-1").unwrap());
    assert!(
        shown.contains("comments (2, newest first)") && shown.contains("Any update?"),
        "{shown}"
    );
    // Nothing changed in Jira: the details aren't read again.
    sync(&jira, &ws).await.unwrap();
    assert_eq!(jira.evidence_reads(), 1);
}

#[tokio::test]
async fn a_round_started_under_the_old_filter_never_fills_the_board() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().to_path_buf();
    let mut jira = Workflow::new();
    // The board shows everyone's; `/board jira BSI me` lands while
    // the background round's search is out.
    linked(&ws, None);
    jira.relink_during_search = Some(ws.clone());
    let r = sync(&jira, &ws).await.unwrap();
    assert_eq!(r.pulled, 0);
    assert_eq!(Board::load(&ws).in_space(Space::Jira).count(), 0);
    // The next round answers the new filter.
    jira.relink_during_search = None;
    sync(&jira, &ws).await.unwrap();
    let b = Board::load(&ws);
    let keys: Vec<&str> = b.in_space(Space::Jira).map(|c| c.key.as_str()).collect();
    assert_eq!(keys, vec!["BSI-1"]);
}

#[tokio::test]
async fn moves_fill_the_resolution_and_review_stays_without_a_review_status() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().to_path_buf();
    let jira = Workflow::new();
    linked(&ws, Some("me"));
    sync(&jira, &ws).await.unwrap();

    // Review: the workflow has none — no call, the card stays in Review.
    Board::update(&ws, |b| {
        b.card_mut("BSI-1")
            .unwrap()
            .set_status(Status::Review, "you")
    })
    .unwrap();
    let r = sync(&jira, &ws).await.unwrap();
    assert!(r.errors.is_empty(), "{r:?}");
    assert!(jira.called(TRANSITION).is_empty());
    let c = Board::load(&ws).card("BSI-1").unwrap().clone();
    assert_eq!(c.status, Status::Review);
    assert!(
        c.notes.iter().any(|n| n.text.contains("no Review status")),
        "{:?}",
        c.notes
    );

    // Done: Resolve Issue, with a resolution (Fixed — there's no Done).
    Board::update(&ws, |b| {
        b.card_mut("BSI-1").unwrap().set_status(Status::Done, "you")
    })
    .unwrap();
    let r = sync(&jira, &ws).await.unwrap();
    assert!(r.errors.is_empty(), "{r:?}");
    assert_eq!(r.moved, 1);
    let t = &jira.called(TRANSITION)[0];
    assert_eq!(t["transitionId"], "5");
    assert_eq!(t["fields"]["resolution"], json!({"id": "10000"}));
    let c = Board::load(&ws).card("BSI-1").unwrap().clone();
    assert_eq!(c.status, Status::Done);
    assert_eq!(c.remote.unwrap().status, "Resolved");

    // Back to To do: Stop Progress, no fields.
    Board::update(&ws, |b| {
        b.card_mut("BSI-1").unwrap().set_status(Status::Todo, "you")
    })
    .unwrap();
    sync(&jira, &ws).await.unwrap();
    let t = &jira.called(TRANSITION)[1];
    assert_eq!(t["transitionId"], "301");
    assert!(t["fields"].is_null());
}

#[test]
fn me_asks_jira_for_the_signed_in_person() {
    assert!(assignee_jql("BSI", "me").contains("assignee = currentUser() AND"));
    assert!(assignee_jql("BSI", "ME").contains("currentUser()"));
    assert!(is_me("currentUser()"));
    assert!(!is_me("Dhanush"));
}

#[tokio::test]
async fn a_transition_picked_by_hand_goes_with_its_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().to_path_buf();
    let jira = Workflow::new();
    linked(&ws, Some("me"));
    sync(&jira, &ws).await.unwrap();
    // The details brought where it can go.
    let b = Board::load(&ws);
    let r = b.card("BSI-1").unwrap().remote.clone().unwrap();
    let labels: Vec<String> = r.transitions.iter().map(|t| t.label()).collect();
    assert_eq!(
        labels,
        vec![
            "Stop Progress → Open",
            "Close Issue → Closed",
            "Resolve Issue → Resolved"
        ]
    );
    assert!(
        b.render_card(b.card("BSI-1").unwrap())
            .contains("can move: 1 Stop Progress → Open")
    );

    // Close, not Resolve (which a move to Done would pick).
    Board::update(&ws, |b| {
        b.card_mut("BSI-1")
            .unwrap()
            .remote
            .as_mut()
            .unwrap()
            .want_transition = "close issue".into()
    })
    .unwrap();
    assert_eq!(Board::load(&ws).pending(), 1);
    let r = sync(&jira, &ws).await.unwrap();
    assert!(r.errors.is_empty(), "{r:?}");
    assert_eq!(r.moved, 1);
    let t = &jira.called(TRANSITION)[0];
    assert_eq!(t["transitionId"], "2");
    assert_eq!(t["fields"]["resolution"], json!({"id": "10000"}));
    let c = Board::load(&ws).card("BSI-1").unwrap().clone();
    assert_eq!(c.status, Status::Done);
    let rm = c.remote.unwrap();
    assert_eq!(rm.status, "Closed");
    assert!(rm.want_transition.is_empty());

    // A move the workflow doesn't have: said, and not tried again.
    Board::update(&ws, |b| {
        b.card_mut("BSI-1")
            .unwrap()
            .remote
            .as_mut()
            .unwrap()
            .want_transition = "Fly away".into()
    })
    .unwrap();
    let r = sync(&jira, &ws).await.unwrap();
    assert!(r.errors[0].contains("no move “Fly away”"), "{r:?}");
    let c = Board::load(&ws).card("BSI-1").unwrap().clone();
    assert!(c.remote.unwrap().want_transition.is_empty());
    assert!(
        c.notes
            .iter()
            .any(|n| n.text.contains("it can: Stop Progress → Open")),
        "{:?}",
        c.notes
    );
}

#[tokio::test]
async fn comments_written_here_go_to_the_ticket() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().to_path_buf();
    let jira = Workflow::new();
    linked(&ws, Some("me"));
    sync(&jira, &ws).await.unwrap();
    Board::update(&ws, |b| {
        let r = b.card_mut("BSI-1").unwrap().remote.as_mut().unwrap();
        r.outbox.push(OutComment {
            body: "Deployed to UAT".into(),
            internal: false,
        });
        r.outbox.push(OutComment {
            body: "Needs CAB".into(),
            internal: true,
        });
        r.outbox.push(OutComment {
            body: "please refuse".into(),
            internal: false,
        });
    })
    .unwrap();
    // Waiting comments keep the card through a filter change.
    assert!(!Board::load(&ws).card("BSI-1").unwrap().unsent_nothing());
    let r = sync(&jira, &ws).await.unwrap();
    assert_eq!(r.commented, 2);
    assert_eq!(r.errors.len(), 1, "{r:?}");
    let sent = jira.called(COMMENT);
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[0]["commentBody"], "Deployed to UAT");
    assert!(sent[0]["jsmCommentType"].is_null());
    assert_eq!(sent[1]["jsmCommentType"], "internalNote");
    let c = Board::load(&ws).card("BSI-1").unwrap().clone();
    assert!(c.remote.as_ref().unwrap().outbox.is_empty());
    assert!(
        c.notes
            .iter()
            .any(|n| n.text.contains("couldn't send the comment")
                && n.text.contains("please refuse"))
    );
    assert!(r.summary().contains("commented 2"), "{}", r.summary());
    // Sent comments make the ticket's details read again.
    assert_eq!(jira.evidence_reads(), 2);
}

/// 250 tickets, 100 a page; or endless pages.
struct Paged {
    endless: bool,
    calls: Mutex<Vec<Value>>,
}

#[async_trait::async_trait]
impl Mcp for Paged {
    async fn tools(&self) -> Result<Vec<String>, String> {
        Ok(vec![SEARCH.to_string()])
    }
    async fn call(&self, _tool: &str, args: Value) -> Result<String, String> {
        self.calls.lock().unwrap().push(args.clone());
        let page: usize = args["nextPageToken"]
            .as_str()
            .map_or(0, |t| t.parse().unwrap());
        let total = if self.endless { usize::MAX } else { 250 };
        let issues: Vec<Value> = (page * 100..(page * 100 + 100).min(total))
                .map(|n| json!({"key": format!("BSI-{n}"), "fields": {"summary": "x", "status": {"name": "Open"}, "assignee": {"displayName": "Me"}}}))
                .collect();
        let last = (page + 1) * 100 >= total;
        let mut d = json!({"isLast": last, "issues": issues});
        if !last {
            d["nextPageToken"] = json!((page + 1).to_string());
        }
        Ok(json!({ "data": d }).to_string())
    }
}

#[tokio::test]
async fn every_page_of_a_big_filter_comes_in() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().to_path_buf();
    linked(&ws, None);
    let jira = Paged {
        endless: false,
        calls: Mutex::new(Vec::new()),
    };
    let r = sync(&jira, &ws).await.unwrap();
    assert_eq!(r.pulled, 250);
    assert!(!r.truncated);
    let calls = jira.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[2]["nextPageToken"], "2");
    assert_eq!(Board::load(&ws).in_space(Space::Jira).count(), 250);
}

#[tokio::test]
async fn a_cut_off_pull_forgets_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().to_path_buf();
    linked(&ws, None);
    // A ticket from before, past where the endless filter is read to.
    Board::update(&ws, |b| {
        b.insert_remote(
            Remote {
                key: "BSI-99999".into(),
                ..Default::default()
            },
            "old",
            "",
            Status::Todo,
            Priority::Medium,
            None,
        )
    })
    .unwrap();
    let jira = Paged {
        endless: true,
        calls: Mutex::new(Vec::new()),
    };
    let r = sync(&jira, &ws).await.unwrap();
    assert_eq!(jira.calls.lock().unwrap().len(), MAX_PAGES);
    assert!(r.truncated);
    assert!(r.summary().contains("1000+ tickets"), "{}", r.summary());
    assert!(Board::load(&ws).card("BSI-99999").is_some());
}

#[test]
fn the_sprint_filter_asks_for_open_sprints() {
    let mut l = JiraLink {
        server: "a".into(),
        cloud_id: "c".into(),
        site: String::new(),
        project: "BSI".into(),
        jql: None,
        assignee: Some("me".into()),
        last_sync: None,
        last_error: None,
        fields: Default::default(),
        sprint: true,
        views: Vec::new(),
        view: None,
        daily_summary: true,
        extra: Default::default(),
    };
    assert_eq!(
        pull_jql(&l),
        "project = \"BSI\" AND assignee = currentUser() AND sprint in openSprints() ORDER BY updated DESC"
    );
    l.sprint = false;
    l.assignee = None;
    assert_eq!(pull_jql(&l), project_jql("BSI"));
    l.jql = Some("key = BSI-1".into());
    assert_eq!(pull_jql(&l), "key = BSI-1");
}

#[tokio::test]
async fn what_others_do_to_your_tickets_is_flagged() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().to_path_buf();
    let jira = Workflow::new();
    linked(&ws, Some("me"));
    // The first look is never news.
    let r = sync(&jira, &ws).await.unwrap();
    assert!(r.activity.is_empty(), "{r:?}");
    assert!(
        Board::load(&ws)
            .card("BSI-1")
            .unwrap()
            .remote
            .as_ref()
            .unwrap()
            .activity
            .is_empty()
    );

    // Someone resolves it in Jira and comments.
    for i in jira.issues.lock().unwrap().iter_mut() {
        if i.0 == "BSI-1" {
            i.2 = "Resolved".into();
            i.3 = "Done".into();
        }
    }
    *jira.new_comment.lock().unwrap() = true;
    let r = sync(&jira, &ws).await.unwrap();
    assert_eq!(
        r.activity,
        vec![
            "BSI-1: moved to Resolved".to_string(),
            "BSI-1: Supriya commented".to_string()
        ]
    );
    let c = Board::load(&ws).card("BSI-1").unwrap().clone();
    assert_eq!(c.remote.as_ref().unwrap().activity, "Supriya commented");
    assert_eq!(c.status, Status::Done);
    assert!(
        Board::load(&ws)
            .render_card(&c)
            .contains("● Supriya commented")
    );

    // Nothing new: no news.
    let r = sync(&jira, &ws).await.unwrap();
    assert!(r.activity.is_empty(), "{r:?}");
}

#[test]
fn your_own_comment_is_not_news() {
    let old = Remote {
        detail_of: "t1".into(),
        comment_count: 1,
        ..Default::default()
    };
    let mut d = Details {
        comment_count: 2,
        ..Default::default()
    };
    d.comments.push(dcode_ai_common::board::RemoteComment {
        author: "Dhanush Durai".into(),
        author_id: "712020:me".into(),
        ..Default::default()
    });
    assert_eq!(comment_activity(&old, &d, Some("712020:me")), None);
    assert_eq!(
        comment_activity(&old, &d, Some("712020:other")).as_deref(),
        Some("Dhanush commented")
    );
    // Never on the first read.
    let first = Remote::default();
    assert_eq!(comment_activity(&first, &d, None), None);
}

#[tokio::test]
async fn time_is_logged_and_attachments_are_saved() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().to_path_buf();
    let jira = Workflow::new();
    // A one-shot file server for the short-lived download link.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    *jira.download_url.lock().unwrap() = format!(
        "http://{}/file/1/binary?token=x",
        listener.local_addr().unwrap()
    );
    let server = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 2048];
        let _ = sock.read(&mut buf).await.unwrap();
        sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello")
            .await
            .unwrap();
    });
    linked(&ws, Some("me"));
    sync(&jira, &ws).await.unwrap();
    let r = Board::load(&ws)
        .card("BSI-1")
        .unwrap()
        .remote
        .clone()
        .unwrap();
    assert_eq!(r.attachments.len(), 1);
    assert_eq!(r.attachments[0].filename, "spec.xlsx");
    assert!(
        r.fields.contains(&("time".into(), "spent 4h".into())),
        "{:?}",
        r.fields
    );

    Board::update(&ws, |b| {
        let r = b.card_mut("BSI-1").unwrap().remote.as_mut().unwrap();
        r.worklogs
            .push(parse_worklog("2h 30m reviewed the PR").unwrap());
        r.downloads.push("1058600".into());
    })
    .unwrap();
    assert_eq!(Board::load(&ws).pending(), 1);
    let r = sync(&jira, &ws).await.unwrap();
    assert!(r.errors.is_empty(), "{r:?}");
    assert_eq!(r.logged, 1);
    let w = &jira.called("executeWrite")[0];
    assert_eq!(w["inputs"]["timeSpent"], "2h 30m");
    assert_eq!(w["inputs"]["comment"], "reviewed the PR");
    let saved = ws.join(".dcode-ai/attachments/BSI-1/spec.xlsx");
    assert_eq!(r.downloaded, vec![saved.clone()]);
    assert_eq!(std::fs::read(&saved).unwrap(), b"hello");
    assert!(ws.join(".dcode-ai/attachments/.gitignore").exists());
    server.await.unwrap();
    let c = Board::load(&ws).card("BSI-1").unwrap().clone();
    let rm = c.remote.as_ref().unwrap();
    assert!(rm.worklogs.is_empty() && rm.downloads.is_empty());
    assert!(
        c.notes
            .iter()
            .any(|n| n.text == "logged 2h 30m reviewed the PR")
    );
}

#[test]
fn worklog_times_read_like_jira() {
    let w = parse_worklog("1d 2h fixed the export").unwrap();
    assert_eq!(
        (w.time.as_str(), w.comment.as_str()),
        ("1d 2h", "fixed the export")
    );
    assert_eq!(parse_worklog("1.5H").unwrap().time, "1.5h");
    assert!(parse_worklog("fixed it in 2h").is_none());
    assert!(parse_worklog("0h").is_none());
    assert_eq!(safe_file_name("../../etc/passwd"), "_.._etc_passwd");
    assert_eq!(safe_file_name("a/b\\c.txt"), "a_b_c.txt");
}

#[test]
fn a_saved_view_replaces_the_pull() {
    let mut l = JiraLink {
        server: "a".into(),
        cloud_id: "c".into(),
        site: String::new(),
        project: "BSI".into(),
        jql: None,
        assignee: Some("me".into()),
        last_sync: None,
        last_error: None,
        fields: Default::default(),
        sprint: false,
        views: vec![dcode_ai_common::board::JiraView {
            name: "ops".into(),
            jql: "project in (BSI, OPS) AND assignee = currentUser()".into(),
        }],
        view: Some("OPS".into()),
        daily_summary: true,
        extra: Default::default(),
    };
    assert_eq!(
        pull_jql(&l),
        "project in (BSI, OPS) AND assignee = currentUser()"
    );
    // A view that was removed falls back to the default pull.
    l.view = Some("gone".into());
    assert!(pull_jql(&l).starts_with("project = \"BSI\" AND assignee = currentUser()"));
}

#[tokio::test]
async fn doctor_checks_the_live_link_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().to_path_buf();
    let jira = Workflow::new();
    linked(&ws, Some("me"));
    let link = Board::load(&ws).jira.unwrap();
    let checks = doctor(&jira, &link).await;
    let text: Vec<String> = checks.iter().map(Check::line).collect();
    assert!(checks.iter().all(|c| c.mark != '✗'), "{text:#?}");
    let find = |what: &str| {
        checks
            .iter()
            .find(|c| c.what == what)
            .unwrap()
            .detail
            .clone()
    };
    assert!(
        find("tools").contains("through executeRead/executeWrite"),
        "{text:#?}"
    );
    assert!(find("you").contains("712020:me"));
    assert!(find("pull").starts_with("1 tickets"), "{text:#?}");
    assert!(
        find("workflow").contains("Close Issue → Closed"),
        "{text:#?}"
    );
    assert!(
        find("workflow").contains("a resolution is filled"),
        "{text:#?}"
    );
    // Read-only: nothing that writes was called.
    assert!(jira.called(TRANSITION).is_empty() && jira.called(COMMENT).is_empty());
    assert!(jira.called("executeWrite").is_empty());
}

#[tokio::test]
async fn today_sums_up_the_board() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().to_path_buf();
    let jira = Workflow::new();
    linked(&ws, Some("me"));
    sync(&jira, &ws).await.unwrap();
    let today = chrono::Local::now().date_naive();
    Board::update(&ws, |b| {
        let c = b.card_mut("BSI-1").unwrap();
        c.assignee = Some("Dhanush Durai".into());
        let r = c.remote.as_mut().unwrap();
        r.activity = "Supriya commented".into();
        r.due_date = (today - chrono::Duration::days(2)).to_string();
    })
    .unwrap();
    let lines = today_lines(&Board::load(&ws), today);
    let text = lines.join("\n");
    assert!(lines[0].starts_with("Jira today · 1 tickets"), "{text}");
    assert!(
        text.contains("● New activity (1)") && text.contains("Supriya commented"),
        "{text}"
    );
    assert!(text.contains("overdue since"), "{text}");
    // Mamatha commented last (the fake's newest comment), not the assignee.
    assert!(
        text.contains("Waiting on you") && text.contains("Mamatha Suresh"),
        "{text}"
    );
    assert!(text.contains("▶ In progress (1)"), "{text}");
}
