//! `save_memory` — lets the agent persist durable, cross-session facts.
//!
//! Notes land in the same store as `/memory` and `dcode-ai memory add`
//! (`.dcode-ai/memory.json` by default) and are recalled at session start via
//! the system-prompt section the supervisor injects. Capture + recall together
//! are what make memory real: without this tool the store was write-only from
//! the user's side and the model never saw it.

use chrono::Utc;
use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};
use dcode_ai_core::tools::ToolExecutor;
use std::path::PathBuf;

use crate::memory_store::{MemoryNote, MemoryStore};

/// Note kinds the model may use; anything else is coerced to "fact".
const KINDS: &[&str] = &["preference", "convention", "decision", "fact"];

pub struct SaveMemoryTool {
    memory_path: PathBuf,
    /// `~/.dcode-ai/memory.json`: notes about the user, recalled in every
    /// project.
    user_path: Option<PathBuf>,
    max_notes: usize,
}

impl SaveMemoryTool {
    pub fn new(memory_path: PathBuf, max_notes: usize) -> Self {
        Self {
            memory_path,
            user_path: None,
            max_notes,
        }
    }

    pub fn with_user_path(mut self, path: Option<PathBuf>) -> Self {
        self.user_path = path;
        self
    }
}

/// Where notes about the user live (every project reads them).
pub fn user_memory_path() -> Option<PathBuf> {
    dcode_ai_common::config::dcode_ai_home_dir().map(|h| h.join("memory.json"))
}

#[async_trait::async_trait]
impl ToolExecutor for SaveMemoryTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "save_memory".into(),
            description: "Persist a durable fact so future sessions remember it: user \
preferences (\"prefers tabs\", \"always answer in German\"), project conventions \
(\"tests live in tests/, run with make test\"), or decisions (\"we chose SQLite over \
Postgres because…\"). Saved notes are shown to you at the start of every future \
session: `scope: \"user\"` notes in every project (the user's own preferences and \
habits), `scope: \"project\"` notes in this workspace only. Use it when the user states something worth remembering \
or asks you to remember something. Do NOT use it for transient task state — that \
belongs in the conversation or update_plan."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "The fact to remember, one or two sentences, self-contained (readable without this conversation)."
                    },
                    "kind": {
                        "type": "string",
                        "enum": KINDS,
                        "description": "What sort of memory this is. Defaults to \"fact\"."
                    },
                    "title": {
                        "type": "string",
                        "description": "Optional short label."
                    },
                    "scope": {
                        "type": "string",
                        "enum": ["project", "user"],
                        "description": "\"user\" for facts about the user that hold in any project; \"project\" for this codebase. Defaults to user for preferences, project otherwise."
                    }
                },
                "required": ["content"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let content = call
            .input
            .get("content")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or_default();
        if content.is_empty() {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some("save_memory requires non-empty 'content'".into()),
            };
        }
        let kind = call
            .input
            .get("kind")
            .and_then(|v| v.as_str())
            .filter(|k| KINDS.contains(k))
            .unwrap_or("fact")
            .to_string();
        let title = call
            .input
            .get("title")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(String::from);

        let user_scope = match call.input.get("scope").and_then(|v| v.as_str()) {
            Some("user") => true,
            Some("project") => false,
            _ => kind == "preference",
        };
        let (path, scope) = match (&self.user_path, user_scope) {
            (Some(p), true) => (p.clone(), "user"),
            _ => (self.memory_path.clone(), "project"),
        };

        let note = MemoryNote {
            id: format!("{kind}-{}", Utc::now().timestamp_millis()),
            created_at: Utc::now(),
            kind: kind.clone(),
            title,
            content: content.to_string(),
        };
        let store = MemoryStore::new(&path);
        match store.append_note(note, self.max_notes).await {
            Ok(state) => ToolResult {
                call_id: call.id.clone(),
                success: true,
                output: format!(
                    "Remembered ({kind}, {scope}): {content}\n({} {scope} note(s) stored; recalled at the start of future sessions)",
                    state.notes.len()
                ),
                error: None,
            },
            Err(err) => ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(format!("failed to save memory: {err}")),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(input: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "t1".into(),
            name: "save_memory".into(),
            input,
        }
    }

    #[tokio::test]
    async fn saves_note_and_reloads_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("memory.json");
        let tool = SaveMemoryTool::new(path.clone(), 8);

        let result = tool
            .execute(&call(serde_json::json!({
                "content": "User prefers 2-space indent",
                "kind": "preference",
            })))
            .await;
        assert!(result.success, "{:?}", result.error);

        let state = MemoryStore::new(&path).load().await.expect("load");
        assert_eq!(state.notes.len(), 1);
        assert_eq!(state.notes[0].kind, "preference");
        assert_eq!(state.notes[0].content, "User prefers 2-space indent");
    }

    #[tokio::test]
    async fn preferences_go_to_user_memory_unless_scoped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let project = dir.path().join("project.json");
        let user = dir.path().join("user.json");
        let tool = SaveMemoryTool::new(project.clone(), 8).with_user_path(Some(user.clone()));
        for input in [
            serde_json::json!({"content": "prefers terse answers", "kind": "preference"}),
            serde_json::json!({"content": "tests run with make test", "kind": "convention"}),
            serde_json::json!({"content": "uses vim", "scope": "user"}),
            serde_json::json!({"content": "tabs here", "kind": "preference", "scope": "project"}),
        ] {
            assert!(tool.execute(&call(input)).await.success);
        }
        let u = MemoryStore::new(&user).load().await.unwrap();
        let p = MemoryStore::new(&project).load().await.unwrap();
        assert_eq!(u.notes.len(), 2);
        assert_eq!(p.notes.len(), 2);
    }

    #[tokio::test]
    async fn rejects_empty_content_and_coerces_unknown_kind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tool = SaveMemoryTool::new(dir.path().join("m.json"), 8);

        let empty = tool
            .execute(&call(serde_json::json!({ "content": "  " })))
            .await;
        assert!(!empty.success);

        let odd = tool
            .execute(&call(serde_json::json!({
                "content": "x",
                "kind": "banana",
            })))
            .await;
        assert!(odd.success);
        let state = MemoryStore::new(dir.path().join("m.json"))
            .load()
            .await
            .unwrap();
        assert_eq!(state.notes[0].kind, "fact");
    }
}
