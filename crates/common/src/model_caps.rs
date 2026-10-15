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
                || m.contains("claude-haiku-4")
                || m.contains("claude-opus-5")
                || m.contains("claude-sonnet-5")
                || m.contains("claude-haiku-5")
                || m.contains("claude-fable-5")
                || m.contains("claude-mythos-5")
        }
        // Copilot's vision models (requests carry `Copilot-Vision-Request`).
        ProviderKind::Copilot => {
            m.contains("claude")
                || m.contains("gpt-4o")
                || m.contains("gpt-4.1")
                || m.contains("gpt-5")
                || m.contains("gemini")
                || m.starts_with("o3")
                || m.starts_with("o4")
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
    fn copilot_vision_models_take_images() {
        assert!(model_accepts_native_images(
            ProviderKind::Copilot,
            "claude-sonnet-5"
        ));
        assert!(model_accepts_native_images(
            ProviderKind::Copilot,
            "gpt-5-mini"
        ));
        assert!(!model_accepts_native_images(
            ProviderKind::Copilot,
            "gpt-3.5-turbo"
        ));
    }
}
