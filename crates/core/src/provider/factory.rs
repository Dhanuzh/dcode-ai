use dcode_ai_common::config::{AnthropicBackend, DcodeAiConfig, ProviderKind};

use super::anthropic::AnthropicProvider;
use super::claude_code::ClaudeCodeProvider;
use super::copilot::CopilotProvider;
use super::{Provider, ProviderError};

/// Build the configured provider for the current workspace.
pub fn build_provider(config: &DcodeAiConfig) -> Result<Box<dyn Provider>, ProviderError> {
    match config.provider.default {
        // Claude account via the user's own `claude` binary (default), or the
        // Messages API with `ant` OAuth. No silent fallback between them.
        ProviderKind::Anthropic => match config.provider.anthropic.backend {
            AnthropicBackend::ClaudeCode => Ok(Box::new(ClaudeCodeProvider::from_config(config)?)),
            AnthropicBackend::Api => Ok(Box::new(AnthropicProvider::from_config(config)?)),
        },
        ProviderKind::Copilot => Ok(Box::new(CopilotProvider::from_config(config)?)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_builds_anthropic_api_backend_with_key() {
        let mut config = DcodeAiConfig::default();
        config.provider.default = ProviderKind::Anthropic;
        config.provider.anthropic.backend = AnthropicBackend::Api;
        config.provider.anthropic.api_key = Some("sk-ant-api03-test".into());
        assert!(build_provider(&config).is_ok());
    }
}
