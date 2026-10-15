//! `tool_search`: find deferred tools (usually MCP tools, when a session has
//! many) and load their schemas. A found tool is activated, so its schema is
//! sent with every later request and the model can call it.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};

use super::ToolExecutor;

/// Names of deferred tools that `tool_search` has loaded.
#[derive(Clone, Default)]
pub struct Activated(Arc<Mutex<HashSet<String>>>);

impl Activated {
    pub fn names(&self) -> HashSet<String> {
        self.0.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn add(&self, name: &str) {
        if let Ok(mut s) = self.0.lock() {
            s.insert(name.to_string());
        }
    }
}

pub struct ToolSearchTool {
    deferred: Vec<ToolDefinition>,
    activated: Activated,
}

/// Most tools one search loads.
const MAX_RESULTS: usize = 5;

impl ToolSearchTool {
    pub fn new(deferred: Vec<ToolDefinition>, activated: Activated) -> Self {
        Self {
            deferred,
            activated,
        }
    }

    /// Matching tools: `select:a,b` by exact name, otherwise ranked by how
    /// many query words appear in the name (weighted) and description.
    fn search(&self, query: &str) -> Vec<&ToolDefinition> {
        let q = query.trim();
        if let Some(names) = q.strip_prefix("select:") {
            let wanted: Vec<&str> = names.split(',').map(str::trim).collect();
            return self
                .deferred
                .iter()
                .filter(|d| wanted.contains(&d.name.as_str()))
                .collect();
        }
        let words: Vec<String> = q
            .to_ascii_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 1)
            .map(str::to_string)
            .collect();
        let mut scored: Vec<(usize, &ToolDefinition)> = self
            .deferred
            .iter()
            .map(|d| {
                let name = d.name.to_ascii_lowercase();
                let desc = d.description.to_ascii_lowercase();
                let score = words
                    .iter()
                    .map(|w| {
                        3 * usize::from(name.contains(w.as_str()))
                            + usize::from(desc.contains(w.as_str()))
                    })
                    .sum();
                (score, d)
            })
            .filter(|(score, _)| *score > 0)
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
        // Keep the strong matches: at least half the best score.
        let best = scored.first().map(|(s, _)| *s).unwrap_or(0);
        scored
            .into_iter()
            .filter(|(score, _)| score * 2 >= best)
            .take(MAX_RESULTS)
            .map(|(_, d)| d)
            .collect()
    }
}

#[async_trait::async_trait]
impl ToolExecutor for ToolSearchTool {
    fn definition(&self) -> ToolDefinition {
        let names: Vec<&str> = self.deferred.iter().map(|d| d.name.as_str()).collect();
        ToolDefinition {
            name: "tool_search".into(),
            description: format!(
                "Load deferred tools before using them. These tools exist but their schemas are \
not loaded yet: {}. Search with keywords (`query: \"flow run history\"`) or exact names \
(`query: \"select:name1,name2\"`). Matching tools' full schemas are returned and they become \
callable from your next step.",
                names.join(", ")
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Keywords, or `select:<name>[,<name>…]` for exact tools"
                    }
                },
                "required": ["query"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let query = call.input["query"].as_str().unwrap_or("");
        let found = self.search(query);
        let output = if found.is_empty() {
            format!("No deferred tool matches `{query}`.")
        } else {
            let mut out = String::from("Loaded these tools; call them by name:\n");
            for d in &found {
                self.activated.add(&d.name);
                out.push_str(&format!(
                    "\n{}\n{}\nparameters: {}\n",
                    d.name, d.description, d.parameters
                ));
            }
            out
        };
        ToolResult {
            call_id: call.id.clone(),
            success: true,
            output,
            error: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::ToolRegistry;
    use super::*;
    use serde_json::json;

    struct Fake(&'static str, &'static str);

    #[async_trait::async_trait]
    impl ToolExecutor for Fake {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: self.0.into(),
                description: self.1.into(),
                parameters: json!({"type": "object"}),
            }
        }
        async fn execute(&self, call: &ToolCall) -> ToolResult {
            ToolResult {
                call_id: call.id.clone(),
                success: true,
                output: format!("ran {}", self.0),
                error: None,
            }
        }
    }

    fn call(name: &str, input: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "c".into(),
            name: name.into(),
            input,
        }
    }

    #[tokio::test]
    async fn deferred_tools_load_on_search_and_stay_callable() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(Fake("read_file", "Read a file")));
        reg.register(Box::new(Fake(
            "mcp__pa__get_flow",
            "Get a Power Automate flow definition",
        )));
        reg.register(Box::new(Fake(
            "mcp__pa__list_runs",
            "List recent runs of a flow",
        )));
        assert_eq!(reg.defer_behind_search(|n| n.starts_with("mcp__")), 2);

        let names = |r: &ToolRegistry| -> Vec<String> {
            r.definitions().into_iter().map(|d| d.name).collect()
        };
        assert_eq!(names(&reg), vec!["read_file", "tool_search"]);
        // Deferred tools still execute when called by name.
        assert_eq!(
            reg.execute(&call("mcp__pa__list_runs", json!({})))
                .await
                .output,
            "ran mcp__pa__list_runs"
        );

        let found = reg
            .execute(&call("tool_search", json!({"query": "flow definition"})))
            .await
            .output;
        assert!(found.contains("mcp__pa__get_flow"), "{found}");
        assert!(names(&reg).contains(&"mcp__pa__get_flow".to_string()));
        assert!(!names(&reg).contains(&"mcp__pa__list_runs".to_string()));

        reg.execute(&call(
            "tool_search",
            json!({"query": "select:mcp__pa__list_runs"}),
        ))
        .await;
        assert_eq!(names(&reg).len(), 4);
    }
}
