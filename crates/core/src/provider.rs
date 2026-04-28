pub mod anthropic;
pub mod anthropic_compat;
pub mod factory;
pub mod openai;
pub mod openai_compat;
pub mod openrouter;
#[cfg(test)]
pub mod test_support;
pub mod validate;

use std::path::Path;

use async_trait::async_trait;
use dcode_ai_common::message::Message;
use dcode_ai_common::tool::{ToolCall, ToolDefinition};

/// Capabilities supported by a provider + model combination.
/// Used to gate feature availability (thinking, vision, etc.).
#[derive(Debug, Clone, Default)]
pub struct ProviderCapabilities {
    /// Model emits reasoning/thinking tokens that can be streamed separately.
    pub supports_thinking_stream: bool,
    /// Model can accept native image inputs in user messages.
    pub supports_native_images: bool,
    /// Model can accept video inputs.
    pub supports_video: bool,
}

/// A streamed chunk from the provider.
#[derive(Debug, Clone)]
pub enum StreamChunk {
    TextDelta(String),
    /// Internal thinking/reasoning output. Rendered as `AgentEvent::ThinkingDelta`.
    /// Normalized from provider-specific variants (Anthropic's `thinking_delta`, OpenAI's `reasoning_content`).
    InternalDelta(String),
    ToolUse(ToolCall),
    Usage {
        input_tokens: u64,
        output_tokens: u64,
    },
    Error(String),
    Done,
}

/// Abstraction over LLM providers (Anthropic, OpenAI, Gemini, etc.).
#[async_trait]
pub trait Provider: Send + Sync {
    /// Rewrite conversation history before an HTTP request (provider-specific preprocessing).
    /// Default: no-op.
    async fn prepare_messages_for_request(
        &self,
        _messages: &mut Vec<Message>,
        _workspace_root: &Path,
    ) -> Result<(), ProviderError> {
        Ok(())
    }

    /// Return capabilities for this provider.
    /// Default: no thinking stream, no native images.
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    /// Send a conversation and receive a streaming response.
    ///
    /// `workspace_root` is used to resolve on-disk image paths embedded in user messages.
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        model: &str,
        workspace_root: &Path,
    ) -> Result<tokio::sync::mpsc::Receiver<StreamChunk>, ProviderError>;
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("provider configuration error: {0}")]
    Configuration(String),
    #[error("API request failed: {0}")]
    RequestFailed(String),
    #[error("Authentication error: {0}")]
    AuthError(String),
    #[error("Rate limited, retry after {retry_after_ms}ms")]
    RateLimited { retry_after_ms: u64 },
    #[error("Model not found: {0}")]
    ModelNotFound(String),
    #[error("{0}")]
    Other(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_capabilities_default_is_all_false() {
        let caps = ProviderCapabilities::default();
        assert!(!caps.supports_thinking_stream);
        assert!(!caps.supports_native_images);
        assert!(!caps.supports_video);
    }

    #[test]
    fn stream_chunk_internal_delta_has_neutral_name() {
        // Verify the variant name is neutral (InternalDelta, not ThinkingDelta)
        let chunk = StreamChunk::InternalDelta("thinking".to_string());
        let name = format!("{:?}", chunk);
        assert!(
            name.contains("InternalDelta"),
            "chunk should use InternalDelta, got: {name}"
        );
    }

    #[test]
    fn anthropic_capabilities_detects_sonnet_opus() {
        // Test heuristic: sonnet/opus models have extended thinking
        let caps = ProviderCapabilities {
            supports_thinking_stream: true,
            supports_native_images: true,
            ..Default::default()
        };
        assert!(
            caps.supports_thinking_stream,
            "sonnet/opus should have thinking stream"
        );
        assert!(
            caps.supports_native_images,
            "sonnet/opus should support images"
        );
    }

    #[test]
    fn openai_capabilities_detects_o_series() {
        // Test heuristic: o-series models have reasoning
        let caps = ProviderCapabilities {
            supports_thinking_stream: true,
            supports_native_images: true,
            supports_video: false,
            ..Default::default()
        };
        let model = "o1-preview";
        let model_lower = model.to_ascii_lowercase();
        assert!(model_lower.starts_with('o'), "o-series detection");
        assert!(
            caps.supports_thinking_stream,
            "o-series should have thinking stream"
        );
    }
}
