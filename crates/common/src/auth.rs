use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthStore {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anthropic: Option<ProviderAuth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openai_oauth: Option<OpenAiOAuth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copilot: Option<CopilotAuth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub antigravity: Option<AntigravityAuth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opencodezen_oauth: Option<OpenCodeZenOAuth>,
    /// Last provider selected by successful login.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_provider: Option<LoggedProvider>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoggedProvider {
    Anthropic,
    Openai,
    Copilot,
    Antigravity,
    Opencodezen,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderAuth {
    pub token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiOAuth {
    pub access_token: String,
    pub refresh_token: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AntigravityAuth {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    pub project_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenCodeZenOAuth {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
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
        self.anthropic.is_some()
            || self.openai_oauth.is_some()
            || self.copilot.is_some()
            || self.antigravity.is_some()
            || self.opencodezen_oauth.is_some()
    }
}

pub fn auth_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".dcode-ai")
        .join("auth.json")
}
