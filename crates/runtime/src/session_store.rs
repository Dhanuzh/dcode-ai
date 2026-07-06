use dcode_ai_common::session::{SessionSnapshot, SessionState};
use std::path::{Path, PathBuf};

/// Current session-file schema version. Bump when the on-disk layout changes
/// incompatibly, and add a migration step in `migrate_session_value`.
/// Files without a `schema_version` field predate versioning and read as 0.
pub const SESSION_SCHEMA_VERSION: u32 = 1;

/// Migrate a raw session JSON value from `from_version` to the current
/// schema. Runs before deserialization so old files keep loading after
/// upgrades. Each arm upgrades one step; fall-through chains them.
fn migrate_session_value(_value: &mut serde_json::Value, from_version: u32) {
    // 0 -> 1: versioning introduced; layout unchanged. Future migrations
    // mutate `_value` in place, stepping one version at a time.
    let _ = from_version;
}

/// Persists and loads session state to/from disk.
pub struct SessionStore {
    sessions_dir: PathBuf,
}

impl SessionStore {
    pub fn new(sessions_dir: impl AsRef<Path>) -> Self {
        Self {
            sessions_dir: sessions_dir.as_ref().to_path_buf(),
        }
    }

    pub fn sessions_dir(&self) -> &Path {
        &self.sessions_dir
    }

    pub async fn save(&self, session: &SessionState) -> Result<(), SessionStoreError> {
        let path = self.sessions_dir.join(format!("{}.json", session.meta.id));
        let tmp_path = self
            .sessions_dir
            .join(format!("{}.json.tmp", session.meta.id));
        let mut value = serde_json::to_value(session)
            .map_err(|e| SessionStoreError::Serialize(e.to_string()))?;
        if let Some(obj) = value.as_object_mut() {
            obj.insert("schema_version".into(), SESSION_SCHEMA_VERSION.into());
        }
        let json =
            serde_json::to_vec(&value).map_err(|e| SessionStoreError::Serialize(e.to_string()))?;

        tokio::fs::create_dir_all(&self.sessions_dir)
            .await
            .map_err(|e| SessionStoreError::Io(e.to_string()))?;

        if let Ok(existing) = tokio::fs::read(&path).await
            && existing == json
        {
            return Ok(());
        }

        tokio::fs::write(&tmp_path, &json)
            .await
            .map_err(|e| SessionStoreError::Io(e.to_string()))?;

        tokio::fs::rename(&tmp_path, &path)
            .await
            .map_err(|e| SessionStoreError::Io(e.to_string()))?;

        Ok(())
    }

    pub async fn load(&self, session_id: &str) -> Result<SessionState, SessionStoreError> {
        let path = self.sessions_dir.join(format!("{session_id}.json"));
        let json = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| SessionStoreError::Io(e.to_string()))?;

        let mut value: serde_json::Value = serde_json::from_str(&json)
            .map_err(|e| SessionStoreError::Deserialize(e.to_string()))?;
        let version = value
            .get("schema_version")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        if version > SESSION_SCHEMA_VERSION {
            return Err(SessionStoreError::Deserialize(format!(
                "session {session_id} uses schema v{version}, but this build reads up to \
                 v{SESSION_SCHEMA_VERSION} — upgrade dcode-ai to resume it"
            )));
        }
        if version < SESSION_SCHEMA_VERSION {
            migrate_session_value(&mut value, version);
        }
        serde_json::from_value(value).map_err(|e| SessionStoreError::Deserialize(e.to_string()))
    }

    pub async fn load_snapshot(
        &self,
        session_id: &str,
    ) -> Result<SessionSnapshot, SessionStoreError> {
        self.load(session_id)
            .await
            .map(|session| session.snapshot())
    }

    pub async fn list(&self) -> Result<Vec<String>, SessionStoreError> {
        let mut ids = Vec::new();
        if !self.sessions_dir.exists() {
            return Ok(ids);
        }
        let mut entries = tokio::fs::read_dir(&self.sessions_dir)
            .await
            .map_err(|e| SessionStoreError::Io(e.to_string()))?;

        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| SessionStoreError::Io(e.to_string()))?
        {
            if let Some(name) = entry.file_name().to_str()
                && let Some(id) = name.strip_suffix(".json")
            {
                ids.push(id.to_string());
            }
        }

        Ok(ids)
    }

    /// Delete a session and its event log from disk.
    /// Returns `NotFound` if neither the JSON state file nor the events file exists.
    pub async fn delete(&self, session_id: &str) -> Result<(), SessionStoreError> {
        let json_path = self.sessions_dir.join(format!("{session_id}.json"));
        let events_path = self.sessions_dir.join(format!("{session_id}.events.jsonl"));

        let mut deleted_any = false;

        if json_path.exists() {
            tokio::fs::remove_file(&json_path)
                .await
                .map_err(|e| SessionStoreError::Io(e.to_string()))?;
            deleted_any = true;
        }

        // Best-effort remove of event log
        if events_path.exists() {
            let _ = tokio::fs::remove_file(&events_path).await;
        }

        if !deleted_any {
            return Err(SessionStoreError::NotFound(session_id.to_string()));
        }

        Ok(())
    }

    /// Delete multiple sessions by ID. Returns the IDs that were successfully deleted.
    pub async fn delete_many(
        &self,
        session_ids: &[String],
    ) -> Result<Vec<String>, SessionStoreError> {
        let mut deleted = Vec::new();
        for id in session_ids {
            match self.delete(id).await {
                Ok(()) => deleted.push(id.clone()),
                Err(SessionStoreError::NotFound(_)) => {
                    // Skip sessions that don't exist on disk
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(deleted)
    }

    /// Load all session snapshots from disk in batch.
    /// Returns two lists: successful loads and IDs that could not be loaded.
    pub async fn load_all_snapshots(
        &self,
    ) -> Result<(Vec<SessionSnapshot>, Vec<String>), SessionStoreError> {
        let ids = self.list().await?;
        let mut snapshots = Vec::new();
        let mut unreadable = Vec::new();
        for id in ids {
            match self.load_snapshot(&id).await {
                Ok(snapshot) => snapshots.push(snapshot),
                Err(_) => unreadable.push(id),
            }
        }
        Ok((snapshots, unreadable))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SessionStoreError {
    #[error("IO error: {0}")]
    Io(String),
    #[error("Serialization error: {0}")]
    Serialize(String),
    #[error("Deserialization error: {0}")]
    Deserialize(String),
    #[error("session not found: {0}")]
    NotFound(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use dcode_ai_common::message::Message;
    use dcode_ai_common::session::{SessionMeta, SessionStatus};

    fn make_session_state(id: &str) -> SessionState {
        SessionState {
            meta: SessionMeta {
                id: id.to_string(),
                session_name: None,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                workspace: PathBuf::from("/tmp"),
                model: "test-model".to_string(),
                status: SessionStatus::Completed,
                pid: None,
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
            },
            messages: vec![Message::user("test")],
            total_input_tokens: 0,
            total_output_tokens: 0,
            estimated_cost_usd: 0.0,
        }
    }

    #[tokio::test]
    async fn save_skips_rewrite_when_bytes_are_unchanged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::new(dir.path());
        let session = make_session_state("session-stable");

        store.save(&session).await.expect("first save");
        let path = dir.path().join("session-stable.json");
        let first = tokio::fs::read(&path).await.expect("read first");

        store.save(&session).await.expect("second save");
        let second = tokio::fs::read(&path).await.expect("read second");

        assert_eq!(first, second, "unchanged session should not rewrite bytes");
    }

    #[tokio::test]
    async fn save_stamps_schema_version_and_load_roundtrips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::new(dir.path());
        store.save(&make_session_state("versioned")).await.unwrap();

        let raw = tokio::fs::read_to_string(dir.path().join("versioned.json"))
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            value["schema_version"].as_u64(),
            Some(SESSION_SCHEMA_VERSION as u64)
        );

        let loaded = store.load("versioned").await.expect("load");
        assert_eq!(loaded.meta.id, "versioned");
    }

    #[tokio::test]
    async fn load_accepts_pre_versioning_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::new(dir.path());
        // Simulate a file written before schema_version existed.
        let session = make_session_state("legacy");
        let json = serde_json::to_string(&session).unwrap();
        assert!(!json.contains("schema_version"));
        tokio::fs::write(dir.path().join("legacy.json"), json)
            .await
            .unwrap();

        let loaded = store.load("legacy").await.expect("legacy load");
        assert_eq!(loaded.meta.id, "legacy");
    }

    #[tokio::test]
    async fn load_rejects_future_schema_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::new(dir.path());
        let session = make_session_state("future");
        let mut value = serde_json::to_value(&session).unwrap();
        value["schema_version"] = serde_json::json!(SESSION_SCHEMA_VERSION + 1);
        tokio::fs::write(
            dir.path().join("future.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .await
        .unwrap();

        let err = store.load("future").await.unwrap_err();
        assert!(
            err.to_string().contains("upgrade dcode-ai"),
            "expected future-version error, got: {err}"
        );
    }

    #[tokio::test]
    async fn delete_removes_json_and_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::new(dir.path());
        let session = make_session_state("session-test-1");
        store.save(&session).await.expect("save");

        // Create a dummy events file
        let events_path = dir.path().join("session-test-1.events.jsonl");
        tokio::fs::write(&events_path, b"{}").await.unwrap();
        assert!(events_path.exists(), "events file should exist");

        // Verify session exists
        assert!(store.load("session-test-1").await.is_ok());

        // Delete
        store.delete("session-test-1").await.expect("delete");

        // Verify gone
        assert!(store.load("session-test-1").await.is_err());
        assert!(!events_path.exists(), "events file should be removed");
    }

    #[tokio::test]
    async fn delete_returns_not_found_for_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::new(dir.path());
        let err = store.delete("nonexistent").await.unwrap_err();
        assert!(
            matches!(err, SessionStoreError::NotFound(_)),
            "expected NotFound, got {err}"
        );
    }

    #[tokio::test]
    async fn delete_many_skips_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::new(dir.path());

        let s1 = make_session_state("session-a");
        let s2 = make_session_state("session-b");
        store.save(&s1).await.expect("save a");
        store.save(&s2).await.expect("save b");

        let deleted = store
            .delete_many(&[
                "session-a".into(),
                "session-b".into(),
                "session-missing".into(),
            ])
            .await
            .expect("delete_many");

        assert_eq!(deleted.len(), 2);
        assert!(deleted.contains(&"session-a".to_string()));
        assert!(deleted.contains(&"session-b".to_string()));
    }

    #[tokio::test]
    async fn load_all_snapshots_returns_snapshots() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::new(dir.path());

        store.save(&make_session_state("s1")).await.unwrap();
        store.save(&make_session_state("s2")).await.unwrap();

        let (snapshots, unreadable) = store.load_all_snapshots().await.expect("load_all");
        assert_eq!(snapshots.len(), 2);
        assert!(unreadable.is_empty());
    }
}
