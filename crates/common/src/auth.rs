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
        let raw = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = auth_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let raw = serde_json::to_string_pretty(self)?;
        std::fs::write(path, raw)?;
        Ok(())
    }

    pub fn any_auth_present(&self) -> bool {
        self.anthropic.is_some() || self.copilot.is_some()
    }
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
}
