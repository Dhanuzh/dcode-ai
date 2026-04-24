use std::path::{Path, PathBuf};

/// Tracks the last active session ID for a workspace.
/// The session ID is stored in `.dcode-ai/.last_session` as plain text.
pub struct LastSessionStore {
    path: PathBuf,
}

impl LastSessionStore {
    /// Create a new store at the given path (typically `<workspace>/.dcode-ai/.last_session`).
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Path to the last-session file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Save a session ID as the last active session.
    pub async fn save(&self, session_id: &str) -> Result<(), LastSessionError> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| LastSessionError::Io(e.to_string()))?;
        }
        tokio::fs::write(&self.path, session_id)
            .await
            .map_err(|e| LastSessionError::Io(e.to_string()))?;
        Ok(())
    }

    /// Load the last active session ID, if any.
    pub async fn load(&self) -> Result<Option<String>, LastSessionError> {
        if !self.path.exists() {
            return Ok(None);
        }
        let content = tokio::fs::read_to_string(&self.path)
            .await
            .map_err(|e| LastSessionError::Io(e.to_string()))?;
        let id = content.trim().to_string();
        if id.is_empty() {
            Ok(None)
        } else {
            Ok(Some(id))
        }
    }

    /// Delete the last-session file (e.g., when no sessions remain).
    pub async fn clear(&self) -> Result<(), LastSessionError> {
        if self.path.exists() {
            tokio::fs::remove_file(&self.path)
                .await
                .map_err(|e| LastSessionError::Io(e.to_string()))?;
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LastSessionError {
    #[error("IO error: {0}")]
    Io(String),
}
