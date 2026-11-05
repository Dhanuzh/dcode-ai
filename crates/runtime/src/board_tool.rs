//! The agent's hands on the project's task board (`/board`): view it, add
//! cards (with blockers, milestones and review gates), and move them along.
//! Writes only `<project>/.dcode-ai/board/board.json`.

use std::path::PathBuf;

use dcode_ai_common::board::{Board, NewCard, Priority, Space, Status};
use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};
use dcode_ai_core::tools::ToolExecutor;
use serde_json::{Value, json};

fn ok(call: &ToolCall, output: String) -> ToolResult {
    ToolResult {
        call_id: call.id.clone(),
        success: true,
        output,
        error: None,
    }
}

fn fail(call: &ToolCall, error: impl Into<String>) -> ToolResult {
    ToolResult {
        call_id: call.id.clone(),
        success: false,
        output: String::new(),
        error: Some(error.into()),
    }
}

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|x| !x.is_empty())
}

fn list(v: &Value, k: &str) -> Vec<String> {
    match v.get(k) {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        Some(Value::String(one)) => one
            .split([',', ' '])
            .filter(|x| !x.trim().is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// `board_view`: the whole board, or one card in full.
pub struct BoardViewTool {
    workspace: PathBuf,
}

impl BoardViewTool {
    pub fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for BoardViewTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "board_view".into(),
            description: "Show the project's task board (the kanban the user sees with /board): \
columns Backlog, To do, In progress, Review, Done, each card's key, priority, assignee, open \
blockers, milestone and review gate, and which cards are ready to start. Pass `key` to read one \
card in full (description and notes)."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "key": { "type": "string", "description": "A card key like DCO-3, to read one card in full." }
                }
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let board = Board::load(&self.workspace);
        match s(&call.input, "key") {
            Some(k) => match board.resolve_key(k).and_then(|k| board.card(&k)) {
                Some(c) => ok(call, board.render_card(c)),
                None => fail(call, format!("no card {k} on the board")),
            },
            None => ok(call, board.render_text()),
        }
    }
}

/// `board_add`: new cards, one or many (a plan).
pub struct BoardAddTool {
    workspace: PathBuf,
}

impl BoardAddTool {
    pub fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for BoardAddTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "board_add".into(),
            description:
                "Add cards to the project's task board. Send several at once to lay out a \
plan: each card can name a milestone, the cards it is blocked by (keys of existing cards, or \
`#n` for the n-th card in this same call), a priority, and `gate: true` for a review gate (the \
card then waits in Review until a person moves it to Done). Cards start in To do unless `status` \
says otherwise. Cards are personal (this computer only) unless `space` is `jira`: then each becomes \
a ticket in the linked Jira project at the next sync — use that only when the user asks for Jira \
tickets. Returns the new keys."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "cards": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": { "type": "string" },
                                "description": { "type": "string" },
                                "priority": { "type": "string", "enum": ["low", "medium", "high", "urgent"] },
                                "status": { "type": "string", "enum": ["backlog", "todo", "in_progress", "review", "done"] },
                                "milestone": { "type": "string", "description": "Milestone title (made if new)." },
                                "blocked_by": { "type": "array", "items": { "type": "string" }, "description": "Keys like DCO-2, or #1 for the first card of this call." },
                                "gate": { "type": "boolean" },
                                "assignee": { "type": "string" },
                                "space": { "type": "string", "enum": ["personal", "jira"], "description": "personal (default) or jira (makes a ticket in the linked project)" }
                            },
                            "required": ["title"]
                        }
                    }
                },
                "required": ["cards"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let Some(cards) = call.input.get("cards").and_then(Value::as_array) else {
            return fail(call, "board_add needs a `cards` array");
        };
        if cards.is_empty() || cards.len() > 50 {
            return fail(call, "send 1 to 50 cards");
        }
        let cards = cards.clone();
        let result = Board::update(&self.workspace, |b| {
            let wants_jira = cards
                .iter()
                .any(|c| s(c, "space").and_then(Space::parse) == Some(Space::Jira));
            if wants_jira && b.jira.is_none() {
                return Err(
                    "the board isn't linked to Jira (the user links it with /board jira <PROJECT>)"
                        .to_string(),
                );
            }
            let mut keys: Vec<String> = Vec::new();
            for c in &cards {
                let Some(title) = s(c, "title") else { continue };
                let blocked_by = list(c, "blocked_by")
                    .into_iter()
                    .map(|k| {
                        // `#n`: the n-th card of this call.
                        k.strip_prefix('#')
                            .and_then(|n| n.parse::<usize>().ok())
                            .and_then(|n| keys.get(n.wrapping_sub(1)).cloned())
                            .unwrap_or(k)
                    })
                    .collect();
                let key = b.add(
                    NewCard {
                        title: title.to_string(),
                        description: s(c, "description").unwrap_or("").to_string(),
                        status: s(c, "status").and_then(Status::parse),
                        priority: s(c, "priority").and_then(Priority::parse),
                        assignee: s(c, "assignee").map(str::to_string),
                        blocked_by,
                        milestone: s(c, "milestone").map(str::to_string),
                        gate: c.get("gate").and_then(Value::as_bool).unwrap_or(false),
                        space: s(c, "space").and_then(Space::parse).unwrap_or_default(),
                    },
                    "agent",
                );
                keys.push(key);
            }
            Ok(keys)
        });
        match result {
            Ok(Ok(keys)) if keys.is_empty() => fail(call, "no card had a title"),
            Ok(Ok(keys)) => ok(
                call,
                format!(
                    "Added {}. The user sees them with /board (Jira cards get their ticket key at the next sync).",
                    keys.join(", ")
                ),
            ),
            Ok(Err(e)) => fail(call, e),
            Err(e) => fail(call, format!("could not save the board: {e}")),
        }
    }
}

/// `board_update`: move a card, assign it, note what happened.
pub struct BoardUpdateTool {
    workspace: PathBuf,
}

impl BoardUpdateTool {
    pub fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for BoardUpdateTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "board_update".into(),
            description: "Change a card on the project's task board: its status (move it to \
in_progress when work starts, review when it's ready to check, done when finished), assignee \
(`agent`, a person, or a sub-agent's session id), priority, blockers, the branch working on it, \
or add a note (what was done, results, links). A card with a review gate can't be moved to done \
by you: leave it in review for a person."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "key": { "type": "string" },
                    "status": { "type": "string", "enum": ["backlog", "todo", "in_progress", "review", "done"] },
                    "assignee": { "type": "string" },
                    "priority": { "type": "string", "enum": ["low", "medium", "high", "urgent"] },
                    "blocked_by": { "type": "array", "items": { "type": "string" } },
                    "branch": { "type": "string" },
                    "title": { "type": "string" },
                    "description": { "type": "string" },
                    "note": { "type": "string" }
                },
                "required": ["key"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let Some(key) = s(&call.input, "key") else {
            return fail(call, "board_update needs a card `key`");
        };
        let input = call.input.clone();
        let key = key.to_string();
        let result = Board::update(&self.workspace, |b| -> Result<String, String> {
            let key = b
                .resolve_key(&key)
                .ok_or_else(|| format!("no card {key} on the board"))?;
            let blockers: Option<Vec<String>> = input.get("blocked_by").map(|_| {
                list(&input, "blocked_by")
                    .iter()
                    .filter_map(|k| b.resolve_key(k))
                    .filter(|k| *k != key)
                    .collect()
            });
            let card = b.card_mut(&key).ok_or("card vanished")?;
            let mut changed = Vec::new();
            if let Some(st) = s(&input, "status") {
                let st = Status::parse(st).ok_or_else(|| format!("unknown status {st}"))?;
                if st == Status::Done && card.gate && card.status != Status::Done {
                    return Err(format!(
                        "{key} has a review gate: leave it in review for a person to move to done"
                    ));
                }
                card.set_status(st, "agent");
                changed.push(format!("status {}", st.id()));
            }
            if let Some(a) = s(&input, "assignee") {
                card.assignee = Some(a.to_string());
                changed.push(format!("assignee {a}"));
            }
            if let Some(p) = s(&input, "priority").and_then(Priority::parse) {
                card.priority = p;
                changed.push(format!("priority {}", p.id()));
            }
            if let Some(bl) = blockers {
                changed.push(format!("blocked by [{}]", bl.join(", ")));
                card.blocked_by = bl;
            }
            if let Some(br) = s(&input, "branch") {
                card.branch = Some(br.to_string());
                changed.push(format!("branch {br}"));
            }
            if let Some(t) = s(&input, "title") {
                card.title = t.chars().take(200).collect();
                changed.push("title".into());
            }
            if let Some(d) = s(&input, "description") {
                card.description = d.chars().take(4000).collect();
                changed.push("description".into());
            }
            if let Some(n) = s(&input, "note") {
                card.note("agent", n);
                changed.push("note".into());
            }
            card.updated = chrono::Utc::now();
            if s(&input, "title").is_some() || s(&input, "description").is_some() {
                card.edited();
            }
            if changed.is_empty() {
                return Err("nothing to change".into());
            }
            Ok(format!("{key}: {}", changed.join(", ")))
        });
        match result {
            Ok(Ok(msg)) => ok(call, msg),
            Ok(Err(e)) => fail(call, e),
            Err(e) => fail(call, format!("could not save the board: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, input: Value) -> ToolCall {
        ToolCall {
            id: "c1".into(),
            name: name.into(),
            input,
        }
    }

    #[tokio::test]
    async fn the_agent_plans_and_moves_cards() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("shop");
        std::fs::create_dir_all(&ws).unwrap();
        let add = BoardAddTool::new(ws.clone());
        let r = add
            .execute(&call(
                "board_add",
                json!({"cards": [
                    {"title": "API", "milestone": "Launch"},
                    {"title": "UI", "blocked_by": ["#1"], "gate": true, "priority": "high"}
                ]}),
            ))
            .await;
        assert!(r.success, "{r:?}");
        assert!(r.output.contains("SHO-1, SHO-2"));
        let view = BoardViewTool::new(ws.clone())
            .execute(&call("board_view", json!({})))
            .await;
        assert!(view.output.contains("blocked by SHO-1"), "{}", view.output);
        assert!(view.output.contains("Ready to start: SHO-1"));

        let up = BoardUpdateTool::new(ws.clone());
        let r = up
            .execute(&call(
                "board_update",
                json!({"key": "sho-1", "status": "done", "note": "shipped"}),
            ))
            .await;
        assert!(r.success, "{r:?}");
        // A gated card can't be closed by the agent.
        let r = up
            .execute(&call(
                "board_update",
                json!({"key": "SHO-2", "status": "done"}),
            ))
            .await;
        assert!(!r.success);
        assert!(r.error.unwrap().contains("review gate"));
        let b = Board::load(&ws);
        assert_eq!(b.card("SHO-1").unwrap().status, Status::Done);
        assert_eq!(b.ready()[0].key, "SHO-2");
    }
}
