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
