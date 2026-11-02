use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Sign-ins dcode-ai stores itself. Claude sign-in lives in Claude Code
/// (`claude auth login`) or the `ant` CLI, not here. Entries written by
/// providers that were removed are ignored on load and dropped on save.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthStore {
    /// Legacy minted Anthropic API key (pre-OAuth). Still honored by the
    /// Messages-API backend when `ant` is not installed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anthropic: Option<ProviderAuth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copilot: Option<CopilotAuth>,
    /// Last provider selected by successful login.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_provider: Option<LoggedProvider>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoggedProvider {
    Anthropic,
    Copilot,
    /// A provider that is no longer supported.
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderAuth {
    pub token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopilotAuth {
    pub github_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copilot_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copilot_expires_at: Option<i64>,
}

impl AuthStore {
    pub fn load() -> anyhow::Result<Self> {
        let path = auth_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        // Files written before 1.0.49 could be readable by other users.
        lock_down(&path);
        let raw = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    /// Writes the sign-in tokens readable only by you (0600 from the moment
    /// the file exists, then renamed into place).
    pub fn save(&self) -> anyhow::Result<()> {
        save_to(&auth_path(), self)
    }

    pub fn any_auth_present(&self) -> bool {
        self.anthropic.is_some() || self.copilot.is_some()
    }
}

fn save_to(path: &std::path::Path, store: &AuthStore) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let raw = serde_json::to_string_pretty(store)?;
    let tmp = path.with_extension("json.tmp");
    let _ = std::fs::remove_file(&tmp);
    {
        use std::io::Write as _;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(raw.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    lock_down(path);
    Ok(())
}

/// Owner-only (Unix); a no-op elsewhere.
fn lock_down(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path)
            && meta.permissions().mode() & 0o077 != 0
        {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

pub fn auth_path() -> PathBuf {
    crate::config::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".dcode-ai")
        .join("auth.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_auth_file_with_removed_providers_still_loads() {
        let store: AuthStore = serde_json::from_str(
            r#"{
                "copilot": {"github_token": "gho_x"},
                "antigravity": {"access_token": "a", "refresh_token": "r", "project_id": "p"},
                "openai_oauth": {"access_token": "a", "refresh_token": "r"},
                "preferred_provider": "antigravity"
            }"#,
        )
        .expect("legacy auth.json");
        assert_eq!(store.copilot.expect("copilot kept").github_token, "gho_x");
        assert!(matches!(
            store.preferred_provider,
            Some(LoggedProvider::Other)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn auth_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        save_to(&path, &AuthStore::default()).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "auth.json must be 0600");
        // An older, world-readable file is tightened.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        lock_down(&path);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
