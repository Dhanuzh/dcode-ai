//! `dcode-ai session export|import`: hand a session to a teammate as one
//! file, secrets masked; importing adds it to this workspace's sessions so
//! `/resume` finds it.

use std::path::{Path, PathBuf};

use dcode_ai_common::session::{SessionState, SessionStatus};
use dcode_ai_runtime::session_store::SessionStore;
use serde_json::{Value, json};

const FORMAT: &str = "dcode-session/1";

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
    let store = SessionStore::new(sessions_dir);
    let state = store
        .load(id)
        .await
        .map_err(|e| anyhow::anyhow!("no session `{id}`: {e}"))?;
    let events = std::fs::read_to_string(sessions_dir.join(format!("{id}.events.jsonl")))
        .unwrap_or_default();
    let out = output.unwrap_or_else(|| PathBuf::from(format!("{id}.dcode-session")));
    std::fs::write(&out, serde_json::to_string(&bundle(&state, &events)?)?)?;
    Ok(out)
}

pub async fn import(sessions_dir: &Path, workspace: &Path, file: &Path) -> anyhow::Result<String> {
    let text = std::fs::read_to_string(file)
        .map_err(|e| anyhow::anyhow!("could not read {}: {e}", file.display()))?;
    let value: Value = serde_json::from_str(&text)?;
    let (state, events) = unbundle(&value, workspace)?;
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

    #[test]
    fn rejects_other_files() {
        let err = unbundle(&json!({"format": "other"}), Path::new("/w")).unwrap_err();
        assert!(err.to_string().contains("not a dcode-ai session file"));
    }
}
