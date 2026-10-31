//! `dcode-ai session export|import`: hand a session to a teammate as one
//! file, secrets masked; importing adds it to this workspace's sessions so
//! `/resume` finds it.

use std::path::{Path, PathBuf};

use dcode_ai_common::session::{SessionState, SessionStatus};
use dcode_ai_runtime::session_store::SessionStore;
use serde_json::{Value, json};

const FORMAT: &str = "dcode-session/1";

/// A note for the teammate who picks a session up (`/handoff`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Handoff {
    pub from: String,
    pub at: chrono::DateTime<chrono::Utc>,
    pub note: String,
    /// The agent's open todo list, if it had one.
    pub todo: Option<String>,
    /// Files changed in the working tree when it was handed off.
    pub changed_files: Vec<String>,
}

impl Handoff {
    /// The note as shown to the teammate and given to the agent.
    pub fn text(&self) -> String {
        let mut t = format!(
            "Handoff from {} ({}):\n{}",
            self.from,
            self.at.format("%Y-%m-%d %H:%M UTC"),
            if self.note.trim().is_empty() {
                "(no note)"
            } else {
                self.note.trim()
            }
        );
        if let Some(todo) = self.todo.as_deref().filter(|t| !t.trim().is_empty()) {
            t.push_str(&format!("\n\nOpen items:\n{}", todo.trim()));
        }
        if !self.changed_files.is_empty() {
            t.push_str("\n\nChanged files:\n");
            for f in &self.changed_files {
                t.push_str(&format!("- {f}\n"));
            }
        }
        t
    }
}

/// One session as a shareable bundle: state and event log, masked.
pub fn bundle(state: &SessionState, events: &str) -> anyhow::Result<Value> {
    let masked = dcode_ai_cli::share::mask(&serde_json::to_string(state)?);
    let state: Value = serde_json::from_str(&masked)
        .map_err(|e| anyhow::anyhow!("masking broke the session state: {e}"))?;
    let events: Vec<Value> = events
        .lines()
        .filter_map(|l| serde_json::from_str(&dcode_ai_cli::share::mask(l)).ok())
        .collect();
    Ok(json!({
        "format": FORMAT,
        "exported_at": chrono::Utc::now(),
        "state": state,
        "events": events,
    }))
}

/// A bundle as a session of `workspace`, under a new id.
pub fn unbundle(bundle: &Value, workspace: &Path) -> anyhow::Result<(SessionState, String)> {
    if bundle["format"].as_str() != Some(FORMAT) {
        anyhow::bail!("not a dcode-ai session file ({FORMAT})");
    }
    let mut state: SessionState = serde_json::from_value(bundle["state"].clone())?;
    let id = dcode_ai_runtime::supervisor::session::generate_session_id();
    state.meta.id = id.clone();
    state.meta.workspace = workspace.to_path_buf();
    state.meta.status = SessionStatus::Completed;
    state.meta.pid = None;
    state.meta.socket_path = None;
    state.meta.worktree_path = None;
    state.meta.parent_session_id = None;
    state.meta.child_session_ids.clear();
    let name = state
        .meta
        .session_name
        .take()
        .unwrap_or_else(|| "shared session".into());
    state.meta.session_name = Some(format!("{name} (imported)"));
    let events = bundle["events"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|e| serde_json::to_string(e).ok())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    Ok((state, events))
}

pub async fn export(
    sessions_dir: &Path,
    id: &str,
    output: Option<PathBuf>,
) -> anyhow::Result<PathBuf> {
    export_with(sessions_dir, id, output, None).await
}

/// [`export`] with a handoff note for whoever imports it.
pub async fn export_with(
    sessions_dir: &Path,
    id: &str,
    output: Option<PathBuf>,
    handoff: Option<Handoff>,
) -> anyhow::Result<PathBuf> {
    let store = SessionStore::new(sessions_dir);
    let state = store
        .load(id)
        .await
        .map_err(|e| anyhow::anyhow!("no session `{id}`: {e}"))?;
    let events = std::fs::read_to_string(sessions_dir.join(format!("{id}.events.jsonl")))
        .unwrap_or_default();
    let out = output.unwrap_or_else(|| PathBuf::from(format!("{id}.dcode-session")));
    let mut value = bundle(&state, &events)?;
    if let Some(h) = handoff {
        let masked = dcode_ai_cli::share::mask(&serde_json::to_string(&h)?);
        value["handoff"] = serde_json::from_str(&masked)?;
    }
    std::fs::write(&out, serde_json::to_string(&value)?)?;
    Ok(out)
}

pub async fn import(sessions_dir: &Path, workspace: &Path, file: &Path) -> anyhow::Result<String> {
    let text = std::fs::read_to_string(file)
        .map_err(|e| anyhow::anyhow!("could not read {}: {e}", file.display()))?;
    let value: Value = serde_json::from_str(&text)?;
    let (mut state, mut events) = unbundle(&value, workspace)?;
    // A handoff note: shown at the end of the transcript and given to the
    // agent, so the teammate can just say "continue".
    if let Ok(h) = serde_json::from_value::<Handoff>(value["handoff"].clone()) {
        let text = h.text();
        state
            .messages
            .push(dcode_ai_common::message::Message::user(format!(
                "{text}\n\n(This session was handed over to me; continue from here.)"
            )));
        let notice = serde_json::json!({
            "id": 0,
            "event": {"type": "Notice", "message": format!("⇢ {text}")},
        });
        if !events.is_empty() {
            events.push('\n');
        }
        events.push_str(&notice.to_string());
    }
    std::fs::create_dir_all(sessions_dir)?;
    SessionStore::new(sessions_dir)
        .save(&state)
        .await
        .map_err(|e| anyhow::anyhow!("could not save the session: {e}"))?;
    if !events.is_empty() {
        std::fs::write(
            sessions_dir.join(format!("{}.events.jsonl", state.meta.id)),
            events + "\n",
        )?;
    }
    Ok(state.meta.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dcode_ai_common::message::Message;

    #[tokio::test]
    async fn export_masks_secrets_and_import_makes_a_new_local_session() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let now = chrono::Utc::now();
        let state = SessionState {
            meta: dcode_ai_common::session::SessionMeta {
                id: "s-1".into(),
                session_name: Some("auth fix".into()),
                created_at: now,
                updated_at: now,
                workspace: PathBuf::from("/old/ws"),
                model: "m".into(),
                provider: None,
                status: SessionStatus::Completed,
                pid: Some(42),
                socket_path: None,
                worktree_path: None,
                branch: None,
                base_branch: None,
                parent_session_id: None,
                child_session_ids: Vec::new(),
                inherited_summary: None,
                spawn_reason: None,
                session_summary: None,
                orchestration: None,
                goal: None,
            },
            messages: vec![Message::user(
                "use api_key=sk-ant-abcdefghijklmnop1234 please",
            )],
            total_input_tokens: 0,
            total_output_tokens: 0,
            estimated_cost_usd: 0.0,
        };
        SessionStore::new(&sessions).save(&state).await.unwrap();
        std::fs::write(
            sessions.join("s-1.events.jsonl"),
            "{\"type\":\"MessageReceived\",\"role\":\"user\",\"content\":\"token=abcdef123456\"}\n",
        )
        .unwrap();

        let file = export(&sessions, "s-1", Some(dir.path().join("x.dcode-session")))
            .await
            .unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(!text.contains("sk-ant-abcdefghijklmnop1234"), "{text}");
        assert!(!text.contains("abcdef123456"));

        let ws = dir.path().join("new-ws");
        let other = dir.path().join("other-sessions");
        let id = import(&other, &ws, &file).await.unwrap();
        assert_ne!(id, "s-1");
        let loaded = SessionStore::new(&other).load(&id).await.unwrap();
        assert_eq!(loaded.meta.workspace, ws);
        assert_eq!(
            loaded.meta.session_name.as_deref(),
            Some("auth fix (imported)")
        );
        assert_eq!(loaded.meta.pid, None);
        assert!(other.join(format!("{id}.events.jsonl")).exists());
    }

    #[tokio::test]
    async fn a_handoff_note_reaches_the_teammate_and_the_agent() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let state: SessionState = serde_json::from_value(serde_json::json!({
            "meta": {
                "id": "s-2", "created_at": chrono::Utc::now(), "updated_at": chrono::Utc::now(),
                "workspace": "/old", "model": "m", "status": "completed",
                "pid": null, "socket_path": null, "child_session_ids": []
            },
            "messages": [], "total_input_tokens": 0, "total_output_tokens": 0,
            "estimated_cost_usd": 0.0
        }))
        .unwrap();
        SessionStore::new(&sessions).save(&state).await.unwrap();
        let handoff = Handoff {
            from: "dhanush".into(),
            at: chrono::Utc::now(),
            note: "login works; finish the logout page".into(),
            todo: Some("- [ ] logout page".into()),
            changed_files: vec!["src/auth.rs".into()],
        };
        let file = export_with(
            &sessions,
            "s-2",
            Some(dir.path().join("h.dcode-session")),
            Some(handoff),
        )
        .await
        .unwrap();
        let other = dir.path().join("teammate");
        let id = import(&other, &dir.path().join("ws"), &file).await.unwrap();
        let loaded = SessionStore::new(&other).load(&id).await.unwrap();
        let last = loaded.messages.last().unwrap().content.to_summary_text();
        assert!(last.contains("Handoff from dhanush") && last.contains("finish the logout page"));
        assert!(last.contains("src/auth.rs") && last.contains("logout page"));
        let log = std::fs::read_to_string(other.join(format!("{id}.events.jsonl"))).unwrap();
        assert!(log.contains("\"Notice\"") && log.contains("Handoff from dhanush"));
    }

    #[test]
    fn rejects_other_files() {
        let err = unbundle(&json!({"format": "other"}), Path::new("/w")).unwrap_err();
        assert!(err.to_string().contains("not a dcode-ai session file"));
    }
}
