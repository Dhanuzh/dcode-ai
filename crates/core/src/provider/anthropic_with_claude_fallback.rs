use std::path::Path;

use dcode_ai_common::message::Message;
use dcode_ai_common::tool::ToolDefinition;

use super::anthropic::AnthropicProvider;
use super::claude_cli::ClaudeCliProvider;
use super::{Provider, ProviderCapabilities, ProviderError, StreamChunk};

/// Anthropic provider wrapper that retries via local Claude CLI when
/// API-key billing/auth paths fail in known ways.
pub struct AnthropicWithClaudeFallbackProvider {
    api: AnthropicProvider,
    cli: ClaudeCliProvider,
}

impl AnthropicWithClaudeFallbackProvider {
    pub fn new(api: AnthropicProvider, cli: ClaudeCliProvider) -> Self {
        Self { api, cli }
    }
}

#[async_trait::async_trait]
impl Provider for AnthropicWithClaudeFallbackProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        self.api.capabilities()
    }

    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        model: &str,
        workspace_root: &Path,
    ) -> Result<tokio::sync::mpsc::Receiver<StreamChunk>, ProviderError> {
        match self.api.chat(messages, tools, model, workspace_root).await {
            Ok(stream) => Ok(stream),
            Err(err) if should_fallback_to_cli(&err) => {
                self.cli.chat(messages, tools, model, workspace_root).await
            }
            Err(err) => Err(err),
        }
    }
}

fn should_fallback_to_cli(err: &ProviderError) -> bool {
    match err {
        ProviderError::RateLimited { .. } => true,
        ProviderError::AuthError(message) | ProviderError::RequestFailed(message) => {
            let m = message.to_ascii_lowercase();
            m.contains("credit balance")
                || m.contains("rate limit")
                || m.contains("quota")
                || m.contains("billing")
                || m.contains("insufficient")
                || m.contains("authentication")
                || m.contains("invalid authentication")
        }
        _ => false,
    }
}
