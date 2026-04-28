# Provider Abstraction Leakage Fix

## Problem

`StreamChunk::ThinkingDelta(String)` exposes an Anthropic-specific concept at the core/provider boundary. Both the `AgentLoop` and the CLI rendering layer know about "thinking" as a distinct stream variant, even though:

- OpenAI/OpenRouter uses `reasoning_content` (same semantics, different field)
- Only some models emit thinking/reasoning tokens
- The concept "reasoning/thinking output from the model" is provider-agnostic

The fix is to make capability detection explicit and normalize provider-specific variants to a neutral name.

## Changes

### 1. Add `ProviderCapabilities` to `crates/core/src/provider.rs`

```rust
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
```

### 2. Add `capabilities()` to `Provider` trait

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    // ... existing methods ...

    /// Return capabilities for this provider's active model.
    /// Default: no thinking stream, no native images.
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }
}
```

### 3. Add `InternalDelta(String)` variant to `StreamChunk`

Replace `ThinkingDelta(String)` with `InternalDelta(String)` — neutral name that the agent maps to `AgentEvent::ThinkingDelta` based on the provider's capabilities. Providers emit `InternalDelta` instead of `ThinkingDelta`.

### 4. Update all provider implementations

- `anthropic_compat.rs`: `StreamChunk::ThinkingDelta` → `StreamChunk::InternalDelta`
- `openai_compat.rs`: `StreamChunk::ThinkingDelta` → `StreamChunk::InternalDelta`
- OpenRouter, etc. inherit from openai_compat (no change needed)

### 5. Update `AgentLoop` to map `InternalDelta` conditionally

The agent already checks `dcode_ai_common::model_caps::model_accepts_native_images` for images. Extend to check thinking stream capability:

```rust
match chunk {
    StreamChunk::InternalDelta(delta) => {
        self.emit(AgentEvent::ThinkingDelta { delta }).await;
    }
    // ...
}
```

### 6. Add capability impls per provider

Each provider returns its actual capabilities:

| Provider | `supports_thinking_stream` |
|----------|---------------------------|
| `AnthropicProvider` | `true` (models with extended thinking) |
| `OpenAiProvider` | model-dependent (o-series, deepseek-r1, etc.) |
| `OpenRouterProvider` | model-dependent |
| `MinimaxProvider` | model-dependent |

For OpenAI/OpenRouter: check `model.starts_with("o")` or known reasoning model prefixes.

### 7. Emit `AgentEvent::ThinkingDelta` only when capability is available

In `AgentLoop`, map `InternalDelta` → `AgentEvent::ThinkingDelta` unconditionally (the `InternalDelta` name already signals it's an internal concern). The CLI rendering checks `AgentEvent::ThinkingDelta` unconditionally — no change needed there since the event name is already provider-agnostic.

The key fix: renaming `ThinkingDelta` → `InternalDelta` in the streaming layer means the streaming API no longer leaks Anthropic naming into core.

## Validation

- `cargo test -p dcode-ai-core`
- `cargo test -p dcode-ai-runtime`
- Build `cargo build --release`
