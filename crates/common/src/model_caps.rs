//! Heuristic vision / multimodal support by provider and model id.

use crate::config::ProviderKind;

/// Whether the active provider+model is treated as supporting **native** image inputs
/// in chat (not MCP OCR fallback).
pub fn model_accepts_native_images(kind: ProviderKind, model: &str) -> bool {
    let m = model.trim().to_ascii_lowercase();
    match kind {
        ProviderKind::Anthropic => {
            m.contains("claude-3")
                || m.contains("claude-4")
                || m.contains("claude-opus-4")
                || m.contains("claude-sonnet-4")
        }
        ProviderKind::OpenAi | ProviderKind::Antigravity => {
            m.contains("gpt-4o")
                || m.contains("gpt-4-turbo")
                || m.contains("gpt-5")
                || m.contains("o1")
                || m.contains("o3")
                || m.contains("claude-3")
                || m.contains("claude-4")
                || m.contains("gemini")
                || m.contains("qwen-vl")
                || m.contains("vision")
        }
        ProviderKind::OpenRouter => {
            m.contains("gpt-4o")
                || m.contains("gpt-4-turbo")
                || m.contains("gpt-5")
                || m.contains("claude-3")
                || m.contains("claude-4")
                || m.contains("gemini")
                || m.contains("vision")
                || m.contains("qwen-vl")
        }
        ProviderKind::OpenCodeZen => {
            m.contains("vision") || m.contains("gpt-4o") || m.contains("qwen-vl")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_claude_is_vision() {
        assert!(model_accepts_native_images(
            ProviderKind::Anthropic,
            "claude-3-7-sonnet-latest"
        ));
    }

    #[test]
    fn gpt35_is_not_vision_openai() {
        assert!(!model_accepts_native_images(
            ProviderKind::OpenAi,
            "gpt-3.5-turbo"
        ));
    }

    #[test]
    fn copilot_claude_on_openai_surface_is_vision() {
        assert!(model_accepts_native_images(
            ProviderKind::OpenAi,
            "claude-3-7-sonnet-latest"
        ));
    }
}
