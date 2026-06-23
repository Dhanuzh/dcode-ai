//! Model-specific context window sizes and detection.
//!
//! Different LLM models have vastly different context limits.
//! This module provides detection and defaults for common models.
//!
//! For models routed via [OpenRouter](https://openrouter.ai/models), authoritative
//! per-model `context_length` values are published in the public API:
//! `GET https://openrouter.ai/api/v1/models` (JSON field `context_length` on each entry).

use serde::{Deserialize, Serialize};

/// Context window limits for various LLM models (in tokens).
/// These are approximate and may vary by API version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelContextLimits {
    /// Model identifier pattern (partial match)
    pub pattern: &'static str,
    /// Context window size in tokens
    pub context_window: usize,
    /// Recommended max tokens for the model
    pub max_output_tokens: usize,
}

/// Known model context windows.
/// Order matters — more specific patterns must come before generics.
/// Live API lookups override these when available.
pub const MODEL_CONTEXT_LIMITS: &[ModelContextLimits] = &[
    // ── Anthropic Claude 4.x ──────────────────────────────────────────
    ModelContextLimits {
        pattern: "claude-opus-4",
        context_window: 200_000,
        max_output_tokens: 32_000,
    },
    ModelContextLimits {
        pattern: "claude-sonnet-4",
        context_window: 200_000,
        max_output_tokens: 16_000,
    },
    ModelContextLimits {
        pattern: "claude-haiku-4",
        context_window: 200_000,
        max_output_tokens: 8192,
    },
    ModelContextLimits {
        pattern: "claude-fable-5",
        context_window: 200_000,
        max_output_tokens: 16_000,
    },
    // ── Anthropic Claude 3.x ──────────────────────────────────────────
    ModelContextLimits {
        pattern: "claude-3-7",
        context_window: 200_000,
        max_output_tokens: 8192,
    },
    ModelContextLimits {
        pattern: "claude-3-5",
        context_window: 200_000,
        max_output_tokens: 8192,
    },
    ModelContextLimits {
        pattern: "claude-3-opus",
        context_window: 200_000,
        max_output_tokens: 4096,
    },
    ModelContextLimits {
        pattern: "claude-3-sonnet",
        context_window: 200_000,
        max_output_tokens: 4096,
    },
    ModelContextLimits {
        pattern: "claude-3-haiku",
        context_window: 200_000,
        max_output_tokens: 4096,
    },
    // ── OpenAI GPT-4.1 ───────────────────────────────────────────────
    ModelContextLimits {
        pattern: "gpt-4.1-nano",
        context_window: 1_047_576,
        max_output_tokens: 32_768,
    },
    ModelContextLimits {
        pattern: "gpt-4.1-mini",
        context_window: 1_047_576,
        max_output_tokens: 32_768,
    },
    ModelContextLimits {
        pattern: "gpt-4.1",
        context_window: 1_047_576,
        max_output_tokens: 32_768,
    },
    // ── OpenAI o-series (reasoning) ──────────────────────────────────
    ModelContextLimits {
        pattern: "o4-mini",
        context_window: 200_000,
        max_output_tokens: 100_000,
    },
    ModelContextLimits {
        pattern: "o3-pro",
        context_window: 200_000,
        max_output_tokens: 100_000,
    },
    ModelContextLimits {
        pattern: "o3-mini",
        context_window: 200_000,
        max_output_tokens: 100_000,
    },
    ModelContextLimits {
        pattern: "o3",
        context_window: 200_000,
        max_output_tokens: 100_000,
    },
    ModelContextLimits {
        pattern: "o1-pro",
        context_window: 200_000,
        max_output_tokens: 100_000,
    },
    ModelContextLimits {
        pattern: "o1-mini",
        context_window: 128_000,
        max_output_tokens: 65_536,
    },
    ModelContextLimits {
        pattern: "o1",
        context_window: 200_000,
        max_output_tokens: 100_000,
    },
    // ── OpenAI GPT-4o ────────────────────────────────────────────────
    ModelContextLimits {
        pattern: "gpt-4o-mini",
        context_window: 128_000,
        max_output_tokens: 16_384,
    },
    ModelContextLimits {
        pattern: "gpt-4o",
        context_window: 128_000,
        max_output_tokens: 16_384,
    },
    ModelContextLimits {
        pattern: "gpt-4-turbo",
        context_window: 128_000,
        max_output_tokens: 4096,
    },
    ModelContextLimits {
        pattern: "gpt-4",
        context_window: 128_000,
        max_output_tokens: 4096,
    },
    ModelContextLimits {
        pattern: "gpt-3.5-turbo",
        context_window: 16_385,
        max_output_tokens: 4096,
    },
    // ── MiniMax ──────────────────────────────────────────────────────
    ModelContextLimits {
        pattern: "minimax/minimax-m2.7",
        context_window: 204_800,
        max_output_tokens: 131_072,
    },
    ModelContextLimits {
        pattern: "minimax-m2.7",
        context_window: 204_800,
        max_output_tokens: 131_072,
    },
    ModelContextLimits {
        pattern: "minimax-m2.5",
        context_window: 100_000,
        max_output_tokens: 8192,
    },
    ModelContextLimits {
        pattern: "minimax-m2",
        context_window: 32_000,
        max_output_tokens: 8192,
    },
    ModelContextLimits {
        pattern: "minimax-m1",
        context_window: 32_000,
        max_output_tokens: 8192,
    },
    // ── Google Gemini ────────────────────────────────────────────────
    ModelContextLimits {
        pattern: "gemini-2.5-pro",
        context_window: 1_000_000,
        max_output_tokens: 65_536,
    },
    ModelContextLimits {
        pattern: "gemini-2.5-flash",
        context_window: 1_000_000,
        max_output_tokens: 65_536,
    },
    ModelContextLimits {
        pattern: "gemini-2.0-flash",
        context_window: 1_000_000,
        max_output_tokens: 8192,
    },
    ModelContextLimits {
        pattern: "gemini-1.5-pro",
        context_window: 2_000_000,
        max_output_tokens: 8192,
    },
    ModelContextLimits {
        pattern: "gemini-1.5-flash",
        context_window: 1_000_000,
        max_output_tokens: 8192,
    },
    ModelContextLimits {
        pattern: "gemini-1.5",
        context_window: 1_000_000,
        max_output_tokens: 8192,
    },
    // ── DeepSeek ─────────────────────────────────────────────────────
    ModelContextLimits {
        pattern: "deepseek-r1",
        context_window: 128_000,
        max_output_tokens: 16_384,
    },
    ModelContextLimits {
        pattern: "deepseek-v3",
        context_window: 128_000,
        max_output_tokens: 16_384,
    },
    ModelContextLimits {
        pattern: "deepseek",
        context_window: 128_000,
        max_output_tokens: 8192,
    },
    // ── Meta Llama ───────────────────────────────────────────────────
    ModelContextLimits {
        pattern: "llama-4",
        context_window: 10_000_000,
        max_output_tokens: 16_384,
    },
    ModelContextLimits {
        pattern: "llama-3.3",
        context_window: 128_000,
        max_output_tokens: 8192,
    },
    ModelContextLimits {
        pattern: "llama-3.1-405b",
        context_window: 128_000,
        max_output_tokens: 4096,
    },
    ModelContextLimits {
        pattern: "llama-3.1",
        context_window: 128_000,
        max_output_tokens: 4096,
    },
    ModelContextLimits {
        pattern: "llama-3",
        context_window: 8_192,
        max_output_tokens: 2048,
    },
    // ── Alibaba Qwen ─────────────────────────────────────────────────
    ModelContextLimits {
        pattern: "qwen-3",
        context_window: 128_000,
        max_output_tokens: 8192,
    },
    ModelContextLimits {
        pattern: "qwen-2.5",
        context_window: 128_000,
        max_output_tokens: 8192,
    },
    // ── Mistral ──────────────────────────────────────────────────────
    ModelContextLimits {
        pattern: "mistral-large",
        context_window: 128_000,
        max_output_tokens: 8192,
    },
    ModelContextLimits {
        pattern: "codestral",
        context_window: 256_000,
        max_output_tokens: 8192,
    },
    ModelContextLimits {
        pattern: "mistral",
        context_window: 128_000,
        max_output_tokens: 8192,
    },
    // ── Catch-all ────────────────────────────────────────────────────
    ModelContextLimits {
        pattern: "*",
        context_window: 32_000,
        max_output_tokens: 4096,
    },
];

/// Detect the context window size for a given model name.
pub fn detect_context_window(model: &str) -> usize {
    let model_lower = model.to_lowercase();

    for limit in MODEL_CONTEXT_LIMITS {
        if limit.pattern != "*" && model_lower.contains(&limit.pattern.to_lowercase()) {
            return limit.context_window;
        }
    }

    // Fallback
    32_000
}

/// Detect the max output tokens for a given model name.
pub fn detect_max_output_tokens(model: &str) -> usize {
    let model_lower = model.to_lowercase();

    for limit in MODEL_CONTEXT_LIMITS {
        if limit.pattern != "*" && model_lower.contains(&limit.pattern.to_lowercase()) {
            return limit.max_output_tokens;
        }
    }

    // Fallback
    4096
}

/// Get both context window and max output tokens for a model.
#[derive(Debug, Clone)]
pub struct ModelLimits {
    pub context_window: usize,
    pub max_output_tokens: usize,
}

impl ModelLimits {
    pub fn for_model(model: &str) -> Self {
        Self {
            context_window: detect_context_window(model),
            max_output_tokens: detect_max_output_tokens(model),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_4_family() {
        assert_eq!(detect_context_window("claude-opus-4-8"), 200_000);
        assert_eq!(detect_max_output_tokens("claude-opus-4-8"), 32_000);
        assert_eq!(detect_context_window("claude-sonnet-4-6"), 200_000);
        assert_eq!(detect_context_window("claude-haiku-4-5"), 200_000);
        assert_eq!(detect_context_window("claude-fable-5"), 200_000);
    }

    #[test]
    fn claude_3_family() {
        assert_eq!(detect_context_window("claude-3-7-sonnet-latest"), 200_000);
        assert_eq!(detect_context_window("claude-3-5-sonnet-20241022"), 200_000);
        assert_eq!(detect_context_window("claude-3-opus-20240229"), 200_000);
    }

    #[test]
    fn openai_gpt41() {
        assert_eq!(detect_context_window("gpt-4.1"), 1_047_576);
        assert_eq!(detect_context_window("gpt-4.1-mini"), 1_047_576);
        assert_eq!(detect_context_window("gpt-4.1-nano"), 1_047_576);
    }

    #[test]
    fn openai_o_series() {
        assert_eq!(detect_context_window("o3"), 200_000);
        assert_eq!(detect_context_window("o3-mini"), 200_000);
        assert_eq!(detect_context_window("o4-mini"), 200_000);
        assert_eq!(detect_max_output_tokens("o3"), 100_000);
    }

    #[test]
    fn openai_gpt4o() {
        assert_eq!(detect_context_window("gpt-4o-2024-08-06"), 128_000);
        assert_eq!(detect_context_window("gpt-4o-mini"), 128_000);
        assert_eq!(detect_context_window("gpt-4-turbo-2024-04-09"), 128_000);
    }

    #[test]
    fn minimax() {
        assert_eq!(detect_context_window("MiniMax-M2.7"), 204_800);
        assert_eq!(detect_context_window("minimax/minimax-m2.7"), 204_800);
        assert_eq!(detect_context_window("MiniMax-M2.5"), 100_000);
        assert_eq!(detect_context_window("minimax-m2"), 32_000);
    }

    #[test]
    fn gemini() {
        assert_eq!(detect_context_window("gemini-2.5-pro"), 1_000_000);
        assert_eq!(detect_context_window("gemini-2.5-flash"), 1_000_000);
        assert_eq!(detect_context_window("gemini-2.0-flash"), 1_000_000);
        assert_eq!(detect_context_window("gemini-1.5-pro-latest"), 2_000_000);
    }

    #[test]
    fn deepseek() {
        assert_eq!(detect_context_window("deepseek-r1"), 128_000);
        assert_eq!(detect_context_window("deepseek-v3"), 128_000);
    }

    #[test]
    fn llama() {
        assert_eq!(detect_context_window("llama-4-scout"), 10_000_000);
        assert_eq!(detect_context_window("llama-3.3-70b"), 128_000);
        assert_eq!(detect_context_window("llama-3.1-405b"), 128_000);
    }

    #[test]
    fn mistral() {
        assert_eq!(detect_context_window("mistral-large-latest"), 128_000);
        assert_eq!(detect_context_window("codestral-latest"), 256_000);
    }

    #[test]
    fn fallback() {
        assert_eq!(detect_context_window("unknown-model-xyz"), 32_000);
    }

    #[test]
    fn model_limits_struct() {
        let limits = ModelLimits::for_model("claude-sonnet-4-6");
        assert_eq!(limits.context_window, 200_000);
        assert_eq!(limits.max_output_tokens, 16_000);
    }
}
