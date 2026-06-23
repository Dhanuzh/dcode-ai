//! Token-usage and cost estimation for a session.
//!
//! Pricing is per-model, matched by substring against the active model name
//! (same approach as `model_limits`). Rates are USD per 1M tokens and reflect
//! published list prices; they are estimates for display, not billing.

/// USD-per-1M-token rates for one model family.
#[derive(Debug, Clone, Copy)]
struct ModelPricing {
    /// Substring matched against the model name (longest match wins).
    pattern: &'static str,
    input_per_mtok: f64,
    output_per_mtok: f64,
}

/// Pricing table. Order is irrelevant; the longest matching `pattern` wins so
/// that e.g. `claude-3-5-haiku` beats a hypothetical `claude` catch-all.
const PRICING: &[ModelPricing] = &[
    // ── Anthropic ────────────────────────────────────────────────────
    ModelPricing {
        pattern: "claude-opus-4",
        input_per_mtok: 15.0,
        output_per_mtok: 75.0,
    },
    ModelPricing {
        pattern: "claude-sonnet-4",
        input_per_mtok: 3.0,
        output_per_mtok: 15.0,
    },
    ModelPricing {
        pattern: "claude-haiku-4",
        input_per_mtok: 0.8,
        output_per_mtok: 4.0,
    },
    ModelPricing {
        pattern: "claude-fable-5",
        input_per_mtok: 3.0,
        output_per_mtok: 15.0,
    },
    ModelPricing {
        pattern: "claude-3-7-sonnet",
        input_per_mtok: 3.0,
        output_per_mtok: 15.0,
    },
    ModelPricing {
        pattern: "claude-3-5-sonnet",
        input_per_mtok: 3.0,
        output_per_mtok: 15.0,
    },
    ModelPricing {
        pattern: "claude-3-5-haiku",
        input_per_mtok: 0.8,
        output_per_mtok: 4.0,
    },
    ModelPricing {
        pattern: "claude-3-opus",
        input_per_mtok: 15.0,
        output_per_mtok: 75.0,
    },
    ModelPricing {
        pattern: "claude-3-sonnet",
        input_per_mtok: 3.0,
        output_per_mtok: 15.0,
    },
    ModelPricing {
        pattern: "claude-3-haiku",
        input_per_mtok: 0.25,
        output_per_mtok: 1.25,
    },
    // ── OpenAI GPT-4.1 ──────────────────────────────────────────────
    ModelPricing {
        pattern: "gpt-4.1-nano",
        input_per_mtok: 0.10,
        output_per_mtok: 0.40,
    },
    ModelPricing {
        pattern: "gpt-4.1-mini",
        input_per_mtok: 0.40,
        output_per_mtok: 1.60,
    },
    ModelPricing {
        pattern: "gpt-4.1",
        input_per_mtok: 2.0,
        output_per_mtok: 8.0,
    },
    // ── OpenAI o-series reasoning ────────────────────────────────────
    ModelPricing {
        pattern: "o4-mini",
        input_per_mtok: 1.10,
        output_per_mtok: 4.40,
    },
    ModelPricing {
        pattern: "o3-pro",
        input_per_mtok: 20.0,
        output_per_mtok: 80.0,
    },
    ModelPricing {
        pattern: "o3-mini",
        input_per_mtok: 1.10,
        output_per_mtok: 4.40,
    },
    ModelPricing {
        pattern: "o3",
        input_per_mtok: 10.0,
        output_per_mtok: 40.0,
    },
    ModelPricing {
        pattern: "o1-mini",
        input_per_mtok: 3.0,
        output_per_mtok: 12.0,
    },
    ModelPricing {
        pattern: "o1",
        input_per_mtok: 15.0,
        output_per_mtok: 60.0,
    },
    // ── OpenAI GPT-4o ───────────────────────────────────────────────
    ModelPricing {
        pattern: "gpt-4o-mini",
        input_per_mtok: 0.15,
        output_per_mtok: 0.60,
    },
    ModelPricing {
        pattern: "gpt-4o",
        input_per_mtok: 2.50,
        output_per_mtok: 10.0,
    },
    ModelPricing {
        pattern: "gpt-4-turbo",
        input_per_mtok: 10.0,
        output_per_mtok: 30.0,
    },
    ModelPricing {
        pattern: "gpt-4",
        input_per_mtok: 30.0,
        output_per_mtok: 60.0,
    },
    ModelPricing {
        pattern: "gpt-3.5-turbo",
        input_per_mtok: 0.50,
        output_per_mtok: 1.50,
    },
    // ── Google Gemini ───────────────────────────────────────────────
    ModelPricing {
        pattern: "gemini-2.5-pro",
        input_per_mtok: 1.25,
        output_per_mtok: 10.0,
    },
    ModelPricing {
        pattern: "gemini-2.5-flash",
        input_per_mtok: 0.15,
        output_per_mtok: 0.60,
    },
    ModelPricing {
        pattern: "gemini-2.0-flash",
        input_per_mtok: 0.10,
        output_per_mtok: 0.40,
    },
    ModelPricing {
        pattern: "gemini-1.5-pro",
        input_per_mtok: 1.25,
        output_per_mtok: 5.0,
    },
    ModelPricing {
        pattern: "gemini-1.5-flash",
        input_per_mtok: 0.075,
        output_per_mtok: 0.30,
    },
    // ── DeepSeek ────────────────────────────────────────────────────
    ModelPricing {
        pattern: "deepseek-r1",
        input_per_mtok: 0.55,
        output_per_mtok: 2.19,
    },
    ModelPricing {
        pattern: "deepseek-v3",
        input_per_mtok: 0.27,
        output_per_mtok: 1.10,
    },
    // ── Mistral ─────────────────────────────────────────────────────
    ModelPricing {
        pattern: "mistral-large",
        input_per_mtok: 2.0,
        output_per_mtok: 6.0,
    },
    ModelPricing {
        pattern: "codestral",
        input_per_mtok: 0.30,
        output_per_mtok: 0.90,
    },
];

/// Fallback rates when no pattern matches (Claude Sonnet tier).
const DEFAULT_PRICING: ModelPricing = ModelPricing {
    pattern: "",
    input_per_mtok: 3.0,
    output_per_mtok: 15.0,
};

fn pricing_for(model: &str) -> ModelPricing {
    PRICING
        .iter()
        .filter(|p| model.contains(p.pattern))
        .max_by_key(|p| p.pattern.len())
        .copied()
        .unwrap_or(DEFAULT_PRICING)
}

/// Anthropic prompt-cache multipliers relative to the base input rate:
/// reads are ~0.1x, writes (cache creation) ~1.25x.
const CACHE_READ_MULTIPLIER: f64 = 0.1;
const CACHE_WRITE_MULTIPLIER: f64 = 1.25;

/// Tracks token usage and estimates cost for a session.
#[derive(Debug, Clone, Default)]
pub struct CostTracker {
    /// Fresh (non-cached) input tokens.
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Tokens served from the prompt cache (billed at the cheaper read tier).
    pub cache_read_tokens: u64,
    /// Tokens written to the prompt cache (billed at the higher write tier).
    pub cache_creation_tokens: u64,
    /// Active model name, used to pick the pricing tier.
    pub model: String,
}

impl CostTracker {
    /// Create a tracker that prices usage for `model`.
    pub fn for_model(model: String) -> Self {
        Self {
            model,
            ..Self::default()
        }
    }

    pub fn add(&mut self, input: u64, output: u64) {
        self.input_tokens += input;
        self.output_tokens += output;
    }

    /// Record prompt-cache token usage for a turn.
    pub fn add_cache(&mut self, read: u64, creation: u64) {
        self.cache_read_tokens += read;
        self.cache_creation_tokens += creation;
    }

    /// Total input tokens processed, including cached prefix — for display so
    /// the reported input doesn't appear to drop on a cache hit.
    pub fn total_input_tokens(&self) -> u64 {
        self.input_tokens + self.cache_read_tokens + self.cache_creation_tokens
    }

    /// Rough cost estimate in USD using per-model list pricing, with cached
    /// tokens billed at their cheaper/expensive tiers. Falls back to Claude
    /// Sonnet rates for unknown models.
    pub fn estimated_cost_usd(&self) -> f64 {
        let pricing = pricing_for(&self.model);
        let per_input_tok = pricing.input_per_mtok / 1_000_000.0;
        let input_cost = self.input_tokens as f64 * per_input_tok;
        let cache_read_cost = self.cache_read_tokens as f64 * per_input_tok * CACHE_READ_MULTIPLIER;
        let cache_write_cost =
            self.cache_creation_tokens as f64 * per_input_tok * CACHE_WRITE_MULTIPLIER;
        let output_cost = self.output_tokens as f64 * pricing.output_per_mtok / 1_000_000.0;
        input_cost + cache_read_cost + cache_write_cost + output_cost
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_model_uses_sonnet_default() {
        let mut t = CostTracker::for_model("some-unknown-model".into());
        t.add(1_000_000, 1_000_000);
        // 3.0 input + 15.0 output
        assert!((t.estimated_cost_usd() - 18.0).abs() < 1e-9);
    }

    #[test]
    fn opus_priced_higher_than_haiku() {
        let mut opus = CostTracker::for_model("claude-opus-4-7".into());
        opus.add(1_000_000, 1_000_000);
        let mut haiku = CostTracker::for_model("claude-haiku-4-5".into());
        haiku.add(1_000_000, 1_000_000);
        assert!(opus.estimated_cost_usd() > haiku.estimated_cost_usd());
    }

    #[test]
    fn longest_pattern_wins() {
        // "gpt-4o-mini" must not be priced as "gpt-4o" or "gpt-4".
        let mut mini = CostTracker::for_model("gpt-4o-mini".into());
        mini.add(1_000_000, 0);
        assert!((mini.estimated_cost_usd() - 0.15).abs() < 1e-9);
    }

    #[test]
    fn default_tracker_still_prices() {
        let mut t = CostTracker::default();
        t.add(1_000_000, 0);
        assert!((t.estimated_cost_usd() - 3.0).abs() < 1e-9);
    }

    #[test]
    fn cache_reads_cost_less_than_fresh_input() {
        let mut fresh = CostTracker::for_model("claude-sonnet-4-6".into());
        fresh.add(1_000_000, 0);
        let mut cached = CostTracker::for_model("claude-sonnet-4-6".into());
        cached.add_cache(1_000_000, 0);
        // Same token count, but cache reads are 0.1x.
        assert!(cached.estimated_cost_usd() < fresh.estimated_cost_usd());
        assert!((cached.estimated_cost_usd() - 0.3).abs() < 1e-9);
    }

    #[test]
    fn cache_writes_cost_more_than_fresh_input() {
        let mut writes = CostTracker::for_model("claude-sonnet-4-6".into());
        writes.add_cache(0, 1_000_000);
        // 3.0 * 1.25 = 3.75
        assert!((writes.estimated_cost_usd() - 3.75).abs() < 1e-9);
    }

    #[test]
    fn total_input_includes_cache_buckets() {
        let mut t = CostTracker::default();
        t.add(100, 0);
        t.add_cache(50, 25);
        assert_eq!(t.total_input_tokens(), 175);
    }
}
