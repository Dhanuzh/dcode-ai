use std::path::Path;

use dcode_ai_common::config::DcodeAiConfig;

use super::openai::OpenAiProvider;
use super::{Provider, ProviderError};

/// MiniMax/OpenCode Zen currently uses an OpenAI-compatible transport,
/// but we keep a dedicated adapter so the provider surface can evolve
/// independently and stay explicit in the codebase.
pub struct MiniMaxProvider {
    inner: OpenAiProvider,
}

impl MiniMaxProvider {
    pub fn from_config(config: &DcodeAiConfig) -> Result<Self, ProviderError> {
        Ok(Self {
            inner: OpenAiProvider::from_config(
                config,
                dcode_ai_common::config::ProviderKind::OpenCodeZen,
            )?,
        })
    }
}

#[async_trait::async_trait]
impl Provider for MiniMaxProvider {
    async fn prepare_messages_for_request(
        &self,
        messages: &mut Vec<dcode_ai_common::message::Message>,
        workspace_root: &Path,
    ) -> Result<(), ProviderError> {
        self.inner
            .prepare_messages_for_request(messages, workspace_root)
            .await
    }

    fn capabilities(&self) -> super::ProviderCapabilities {
        self.inner.capabilities()
    }

    async fn chat(
        &self,
        messages: &[dcode_ai_common::message::Message],
        tools: &[dcode_ai_common::tool::ToolDefinition],
        model: &str,
        workspace_root: &Path,
    ) -> Result<tokio::sync::mpsc::Receiver<super::StreamChunk>, ProviderError> {
        self.inner
            .chat(messages, tools, model, workspace_root)
            .await
    }
}
